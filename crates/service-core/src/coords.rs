//! Source coordinate mapping.
//!
//! MNCS source spans are byte offsets; LSP positions are zero-based line /
//! UTF-16 code-unit pairs. This module is the single authoritative translation
//! layer for the whole service: adapters must never convert coordinates
//! themselves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

#[path = "native_coords.rs"]
mod native;

use mncs_syntax::SourceSpan;
use serde::{Deserialize, Serialize};

const NATIVE_CHUNK_BYTES: usize = native::CHUNK_BYTES;

/// A source range in both byte and LSP-style coordinates.
///
/// Lines and characters are zero-based; `character` counts UTF-16 code units,
/// matching the Language Server Protocol. Byte fields always carry the exact
/// underlying span so non-LSP clients can avoid lossy round trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeInfo {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// A single source position in both coordinate systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionInfo {
    pub line: u32,
    pub character: u32,
    pub byte: usize,
}

/// Precomputed line index for one document text.
#[derive(Debug, Clone)]
pub struct PositionMap {
    text_len: usize,
    /// Byte offset where each line starts. Line 0 starts at 0.
    line_starts: Vec<usize>,
    /// Native scalar checkpoints are cached for queried lines only. This
    /// avoids compiling or scanning coordinate maps for unused document lines.
    lines: Arc<RwLock<HashMap<u32, Arc<LineIndex>>>>,
}

#[derive(Debug, Clone)]
struct ChunkSummary {
    byte_start: usize,
    units_before: u64,
    map: Vec<u32>,
    map_length: usize,
}

#[derive(Debug)]
struct LineIndex {
    chunks: Vec<ChunkSummary>,
    units: u64,
    windows: Mutex<HashMap<(usize, usize), Arc<WindowIndex>>>,
}

#[derive(Debug)]
struct WindowIndex {
    byte_start: usize,
    units_before: u64,
    map: Vec<u16>,
    map_length: usize,
}

fn unpack_coordinate(packed: u32) -> (usize, u64) {
    let packed = u64::from(packed);
    ((packed % 2048) as usize, packed / 2048)
}

fn unpack_window_coordinate(packed: u16) -> (usize, u64) {
    let packed = u64::from(packed);
    ((packed % 128) as usize, packed / 128)
}

impl PositionMap {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        // A trailing newline does not open an empty extra line for position
        // purposes: LSP treats "abc\n" as one line (line count 1) whose end
        // position is (0, 3) in UTF-16 terms plus a virtual (1, 0).
        Self {
            text_len: text.len(),
            line_starts,
            lines: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    fn line_start(&self, line: u32) -> Option<usize> {
        self.line_starts.get(line as usize).copied()
    }

    /// Convert a byte offset to a zero-based line / UTF-16 column pair.
    ///
    /// Offsets past the end of text clamp to the final position. Offsets that
    /// fall inside a multi-byte character snap down to the character start.
    pub fn position_of(&self, text: &str, byte: usize) -> PositionInfo {
        let requested = byte.min(self.text_len);
        let line = match self.line_starts.binary_search(&requested) {
            Ok(found) => found,
            Err(insertion) => insertion - 1,
        } as u32;
        let line_index = line as usize;
        let line_start = self.line_starts[line_index];
        let content_end = self.line_end_exclusive(text, line);
        let index = self.line_index(text, line);
        if requested > content_end {
            return PositionInfo {
                line,
                character: (index.units + (requested - content_end) as u64) as u32,
                byte: requested,
            };
        }

        let local_byte = requested - line_start;
        let Some((chunk_index, chunk, relative_byte)) = Self::chunk_for_byte(&index, local_byte)
        else {
            return PositionInfo {
                line,
                character: 0,
                byte: requested,
            };
        };
        let checkpoint_index =
            (relative_byte / native::CHECKPOINT_BYTES).min(chunk.map_length.saturating_sub(1));
        let window = self.window_index(text, line, &index, chunk_index, checkpoint_index);
        let relative_window_byte = local_byte - window.byte_start;
        let packed = *window
            .map
            .get(relative_window_byte)
            .filter(|_| relative_window_byte < window.map_length)
            .expect("MNCS coordinate window covers the queried byte position");
        let (snapped_byte, units) = unpack_window_coordinate(packed);
        PositionInfo {
            line,
            character: (window.units_before + units) as u32,
            byte: line_start + window.byte_start + snapped_byte,
        }
    }

    /// Convert a zero-based line / UTF-16 column pair to a byte offset.
    ///
    /// Out-of-range lines clamp to the last line; out-of-range columns clamp
    /// to the end of the requested line. A column that would land inside a
    /// multi-byte or surrogate-pair character snaps up to the next character
    /// boundary within the same line.
    pub fn offset_of(&self, text: &str, line: u32, character: u32) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let line_start = self.line_starts[line as usize];
        let line_end = self.line_end_exclusive(text, line);
        let index = self.line_index(text, line);
        if character as u64 >= index.units {
            return line_end;
        }
        let Some((chunk_index, chunk)) = Self::chunk_for_column(&index, character as u64) else {
            return line_start.min(line_end);
        };
        let relative_column = (character as u64).saturating_sub(chunk.units_before);
        let checkpoint_index = chunk.map[..chunk.map_length]
            .partition_point(|packed| unpack_coordinate(*packed).1 <= relative_column)
            .saturating_sub(1);
        let window = self.window_index(text, line, &index, chunk_index, checkpoint_index);
        let remaining_units = (character as u64).saturating_sub(window.units_before);
        let relative = window.map[..window.map_length]
            .partition_point(|packed| unpack_window_coordinate(*packed).1 < remaining_units);
        let packed = window.map[relative.min(window.map_length.saturating_sub(1))];
        let (byte, _) = unpack_window_coordinate(packed);
        (line_start + window.byte_start + byte)
            .min(line_end)
            .min(self.text_len)
    }

    fn line_index(&self, text: &str, line: u32) -> Arc<LineIndex> {
        if let Some(index) = self
            .lines
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&line)
            .cloned()
        {
            return index;
        }
        let mut lines = self
            .lines
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lines
            .entry(line)
            .or_insert_with(|| Arc::new(self.build_line_index(text, line)))
            .clone()
    }

    fn build_line_index(&self, text: &str, line: u32) -> LineIndex {
        let line_start = self.line_starts[line as usize];
        let line_end = self.line_end_exclusive(text, line);
        let line_bytes = &text.as_bytes()[line_start..line_end];
        let mut chunks = Vec::new();
        let mut byte_start = 0usize;
        let mut units_before = 0u64;
        while byte_start < line_bytes.len() {
            let target = (line_bytes.len() - byte_start).min(NATIVE_CHUNK_BYTES);
            let progress_end = byte_start + target;
            if line_bytes[byte_start..progress_end]
                .iter()
                .all(u8::is_ascii)
            {
                let mut map = (0..target)
                    .step_by(native::CHECKPOINT_BYTES)
                    .map(|offset| (offset as u64 * 2048 + offset as u64) as u32)
                    .collect::<Vec<_>>();
                map.push((target as u64 * 2048 + target as u64) as u32);
                chunks.push(ChunkSummary {
                    byte_start,
                    units_before,
                    map_length: map.len(),
                    map,
                });
                byte_start += target;
                units_before += target as u64;
                continue;
            }
            let view_end = (byte_start + target + 3).min(line_bytes.len());
            let result = native::position_chunk(&line_bytes[byte_start..view_end], target)
                .unwrap_or_else(|error| panic!("failed to index a source line in MNCS: {error}"));
            assert!(result.bytes > 0, "MNCS coordinate index made no progress");
            chunks.push(ChunkSummary {
                byte_start,
                units_before,
                map: result.map.into_iter().take(result.map_length).collect(),
                map_length: result.map_length,
            });
            byte_start += result.bytes;
            units_before += result.units;
        }
        LineIndex {
            chunks,
            units: units_before,
            windows: Mutex::new(HashMap::new()),
        }
    }

    fn window_index(
        &self,
        text: &str,
        line: u32,
        index: &LineIndex,
        chunk_index: usize,
        checkpoint_index: usize,
    ) -> Arc<WindowIndex> {
        let key = (chunk_index, checkpoint_index);
        let mut windows = index
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        windows
            .entry(key)
            .or_insert_with(|| {
                let chunk = &index.chunks[chunk_index];
                let (checkpoint_byte, checkpoint_units) =
                    unpack_coordinate(chunk.map[checkpoint_index]);
                let byte_start = chunk.byte_start + checkpoint_byte;
                let line_start = self.line_starts[line as usize];
                let line_end = self.line_end_exclusive(text, line) - line_start;
                let window_end = (byte_start + native::WINDOW_INPUT_CAPACITY).min(line_end);
                let source = &text.as_bytes()[line_start + byte_start..line_start + window_end];
                let result = if source.iter().all(u8::is_ascii) {
                    let map = (0..=source.len())
                        .map(|offset| (offset * 128 + offset) as u16)
                        .collect::<Vec<_>>();
                    native::CoordinateWindow {
                        map_length: map.len(),
                        map,
                    }
                } else {
                    native::coordinate_window(source).unwrap_or_else(|error| {
                        panic!("failed to map a source window in MNCS: {error}")
                    })
                };
                Arc::new(WindowIndex {
                    byte_start,
                    units_before: chunk.units_before + checkpoint_units,
                    map: result.map.into_iter().take(result.map_length).collect(),
                    map_length: result.map_length,
                })
            })
            .clone()
    }

    fn chunk_for_byte(
        index: &LineIndex,
        local_byte: usize,
    ) -> Option<(usize, &ChunkSummary, usize)> {
        let at = index
            .chunks
            .partition_point(|chunk| chunk.byte_start <= local_byte);
        let chunk_index = at.checked_sub(1)?;
        let chunk = index.chunks.get(chunk_index)?;
        Some((chunk_index, chunk, local_byte - chunk.byte_start))
    }

    fn chunk_for_column(index: &LineIndex, column: u64) -> Option<(usize, &ChunkSummary)> {
        let at = index
            .chunks
            .partition_point(|chunk| chunk.units_before <= column);
        let chunk_index = at.checked_sub(1)?;
        let chunk = index.chunks.get(chunk_index)?;
        Some((chunk_index, chunk))
    }

    fn line_end_exclusive(&self, text: &str, line: u32) -> usize {
        match self.line_start(line + 1) {
            Some(next) => {
                let mut end = next;
                if end >= 1 && text.as_bytes()[end - 1] == b'\n' {
                    end -= 1;
                    if end >= 1 && text.as_bytes()[end - 1] == b'\r' {
                        end -= 1;
                    }
                }
                end
            }
            None => self.text_len,
        }
    }

    /// Project an authoritative [`SourceSpan`] into both coordinate systems.
    pub fn range_of(&self, text: &str, span: SourceSpan) -> RangeInfo {
        let start = self.position_of(text, span.start);
        let end = self.position_of(text, span.end.max(span.start));
        RangeInfo {
            start_line: start.line,
            start_character: start.character,
            end_line: end.line,
            end_character: end.character,
            start_byte: start.byte,
            end_byte: end.byte,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PositionInfo, PositionMap};

    fn reference_line_starts(text: &str) -> Vec<usize> {
        let mut starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        starts
    }

    fn reference_position(text: &str, requested: usize) -> PositionInfo {
        let mut byte = requested.min(text.len());
        while !text.is_char_boundary(byte) {
            byte -= 1;
        }
        let starts = reference_line_starts(text);
        let line = match starts.binary_search(&byte) {
            Ok(found) => found,
            Err(insertion) => insertion - 1,
        } as u32;
        let line_start = starts[line as usize];
        PositionInfo {
            line,
            character: text[line_start..byte]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>() as u32,
            byte,
        }
    }

    fn reference_offset(text: &str, requested_line: u32, character: u32) -> usize {
        let starts = reference_line_starts(text);
        let line = requested_line.min(starts.len().saturating_sub(1) as u32);
        let start = starts[line as usize];
        let mut end = starts.get(line as usize + 1).copied().unwrap_or(text.len());
        if end > start && text.as_bytes()[end - 1] == b'\n' {
            end -= 1;
            if end > start && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
        }
        let mut units = 0u32;
        let mut offset = start;
        for scalar in text[start..end].chars() {
            if units >= character {
                break;
            }
            units += scalar.len_utf16() as u32;
            offset += scalar.len_utf8();
        }
        offset.min(text.len())
    }

    #[test]
    fn ascii_round_trip() {
        let text = "fn main() {\n    return 0;\n}\n";
        let map = PositionMap::new(text);
        assert_eq!(map.line_count(), 4);

        // "fn main() {\n" is 12 bytes; line 1 starts at 12.
        let info = map.position_of(text, 12);
        assert_eq!(
            info,
            PositionInfo {
                line: 1,
                character: 0,
                byte: 12
            }
        );
        assert_eq!(
            map.position_of(text, 15),
            PositionInfo {
                line: 1,
                character: 3,
                byte: 15
            }
        );
        assert_eq!(map.offset_of(text, 1, 0), 12);
        assert_eq!(map.offset_of(text, 1, 11), 23); // before ';'
    }

    #[test]
    fn utf8_and_utf16_columns_differ_correctly() {
        // 'é' is 2 UTF-8 bytes / 1 UTF-16 unit; '𝄞' is 4 bytes / 2 units.
        let text = "let é = 1;\nlet 𝄞x = 2;\n";
        let map = PositionMap::new(text);

        // Offset of 'x' after the astral char on line 1.
        let x_byte = text.find('x').expect("x present");
        let info = map.position_of(text, x_byte);
        assert_eq!(info.line, 1);
        // "let " = 4 units, '𝄞' = 2 units.
        assert_eq!(info.character, 6);
        assert_eq!(map.offset_of(text, 1, 6), x_byte);

        // Column measured after 'é' on line 0: "let " + 1 unit.
        let eq_byte = text.find('=').expect("= present");
        let info = map.position_of(text, eq_byte);
        assert_eq!(info.line, 0);
        assert_eq!(info.character, 6); // "let é " is 6 UTF-16 units
        assert_eq!(map.offset_of(text, 0, 6), eq_byte);
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        // Bytes: a(0) \r(1) \n(2) b(3) b(4) \r(5) \n(6) c(7) c(8) c(9)
        let text = "a\r\nbb\r\nccc";
        let map = PositionMap::new(text);
        assert_eq!(map.line_count(), 3);
        assert_eq!(
            map.position_of(text, 3),
            PositionInfo {
                line: 1,
                character: 0,
                byte: 3
            }
        );
        assert_eq!(
            map.position_of(text, 4),
            PositionInfo {
                line: 1,
                character: 1,
                byte: 4
            }
        );
        assert_eq!(map.offset_of(text, 1, 0), 3);
        assert_eq!(
            map.position_of(text, 7),
            PositionInfo {
                line: 2,
                character: 0,
                byte: 7
            }
        );
        // End of line 1 excludes the CRLF.
        assert_eq!(map.offset_of(text, 1, 99), 5);
    }

    #[test]
    fn out_of_range_positions_clamp_instead_of_panic() {
        let text = "ab\ncd";
        let map = PositionMap::new(text);
        assert_eq!(map.offset_of(text, 9, 0), 3); // clamped line
        assert_eq!(map.offset_of(text, 0, 99), 2); // clamped column
        assert_eq!(map.offset_of(text, 9, 99), 5);
        let end = map.position_of(text, 999);
        assert_eq!(end.line, 1);
        assert_eq!(end.character, 2);
    }

    #[test]
    fn offsets_inside_multibyte_characters_snap_to_boundaries() {
        let text = "aébc";
        let map = PositionMap::new(text);
        // Offset 2 is inside 'é' (bytes 1..3): snap down to the char start.
        assert_eq!(map.position_of(text, 2).character, 1);
        assert_eq!(map.position_of(text, 2).byte, 1);
        // Querying by UTF-16 column between surrogate halves cannot happen,
        // but a column equal to a full character width must be exact.
        assert_eq!(map.offset_of(text, 0, 2), 3);
    }

    #[test]
    fn empty_text_has_one_empty_line() {
        let map = PositionMap::new("");
        assert_eq!(map.line_count(), 1);
        assert_eq!(map.position_of("", 0).line, 0);
        assert_eq!(map.offset_of("", 0, 0), 0);
        assert_eq!(map.offset_of("", 5, 5), 0);
    }

    #[test]
    fn spans_project_into_both_coordinate_systems() {
        let text = "fn f(value: i64) -> (result: i64) { return value; }";
        let map = PositionMap::new(text);
        let start = text.find("value").expect("needle");
        let span = mncs_syntax::SourceSpan::at(text, start, start + 5);
        let range = map.range_of(text, span);
        assert_eq!(range.start_line, 0);
        assert_eq!(range.start_character, start as u32);
        assert_eq!(range.end_character, (start + 5) as u32);
        assert_eq!(range.start_byte, start);
        assert_eq!(range.end_byte, start + 5);
    }

    #[test]
    fn native_coordinate_kernel_matches_independent_mapping_contract() {
        let mut text = String::new();
        for index in 0..5_100 {
            match index % 97 {
                0 => text.push('𝄞'),
                1 => text.push('é'),
                _ => text.push(char::from(b'a' + (index % 26) as u8)),
            }
        }
        text.push_str("\r\nsecond 𝄞line\n");
        let map = PositionMap::new(&text);

        let mut offsets = vec![0, 1, 2, text.len() - 3, text.len()];
        for boundary in [
            super::native::CHUNK_BYTES,
            super::native::CHUNK_BYTES * 2,
            super::native::CHUNK_BYTES * 3,
            super::native::CHUNK_BYTES * 5,
        ] {
            for delta in [-3isize, -1, 0, 1, 3] {
                let offset = boundary.saturating_add_signed(delta);
                if offset <= text.len() {
                    offsets.push(offset);
                }
            }
        }
        for offset in offsets {
            assert_eq!(
                map.position_of(&text, offset),
                reference_position(&text, offset),
                "byte offset {offset}"
            );
        }

        for line in 0..map.line_count() {
            for column in [0, 1, 2, 64, 127, 1023, 5_101, u32::MAX] {
                assert_eq!(
                    map.offset_of(&text, line, column),
                    reference_offset(&text, line, column),
                    "line {line}, UTF-16 column {column}"
                );
            }
        }
        assert!(!super::native::kernel_artifact_identity()
            .expect("native position kernel compiles and executes")
            .is_empty());

        let ascii = "a".repeat(super::native::CHUNK_BYTES * 2 + 5);
        let ascii_map = PositionMap::new(&ascii);
        for offset in [
            0,
            super::native::CHUNK_BYTES - 1,
            super::native::CHUNK_BYTES,
            super::native::CHUNK_BYTES + 1,
            super::native::CHUNK_BYTES * 2,
            ascii.len(),
        ] {
            assert_eq!(
                ascii_map.position_of(&ascii, offset),
                reference_position(&ascii, offset),
                "ASCII byte offset {offset}"
            );
        }
        for column in [0, 1, super::native::CHUNK_BYTES as u32, u32::MAX] {
            assert_eq!(
                ascii_map.offset_of(&ascii, 0, column),
                reference_offset(&ascii, 0, column),
                "ASCII UTF-16 column {column}"
            );
        }
    }
}
