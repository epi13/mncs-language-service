//! Source coordinate mapping.
//!
//! MNCS source spans are byte offsets; LSP positions are zero-based line /
//! UTF-16 code-unit pairs. This module is the single authoritative translation
//! layer for the whole service: adapters must never convert coordinates
//! themselves.

use mncs_syntax::SourceSpan;
use serde::{Deserialize, Serialize};

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
        let byte = normalize_offset(text, byte);
        let line = match self.line_starts.binary_search(&byte) {
            Ok(found) => found,
            Err(insertion) => insertion - 1,
        } as u32;
        let line_start = self.line_starts[line as usize];
        let prefix = &text[line_start..byte];
        let character = prefix.chars().map(char::len_utf16).sum::<usize>() as u32;
        PositionInfo {
            line,
            character,
            byte,
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
        let mut units = 0u32;
        let mut offset = line_start;
        for current in text[line_start..line_end].chars() {
            if units >= character {
                break;
            }
            units += current.len_utf16() as u32;
            offset += current.len_utf8();
        }
        offset.min(self.text_len)
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

fn normalize_offset(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if text.is_char_boundary(byte) {
        byte
    } else {
        let mut adjusted = byte;
        while adjusted > 0 && !text.is_char_boundary(adjusted) {
            adjusted -= 1;
        }
        adjusted
    }
}

#[cfg(test)]
mod tests {
    use super::{PositionInfo, PositionMap};

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
}
