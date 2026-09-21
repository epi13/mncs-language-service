//! Incremental text-change application.
//!
//! The LSP client may synchronize with full-document replacement or with a
//! sequence of ranged edits. This module is the single place that turns either
//! form into exact new document text. Ranges arrive in LSP coordinates
//! (zero-based line / UTF-16 column); translation goes through the snapshot's
//! [`PositionMap`] so byte/character-boundary handling stays in one layer.
//!
//! Sequential semantics follow LSP: each change in one notification applies to
//! the document state produced by the previous change.

use crate::coords::PositionMap;
use serde::{Deserialize, Serialize};

/// One applied content change: `range == None` is a full-document replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChange {
    pub range: Option<TextRange>,
    pub text: String,
}

/// An LSP-style line/UTF-16 range (end-exclusive, as in the protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// Apply `changes` to `current` in order, returning the exact new text.
///
/// Out-of-range positions clamp exactly like [`PositionMap::offset_of`]; a
/// `start` ordered after `end` is swapped rather than rejected so a quirky
/// client cannot desynchronize the store.
pub fn apply_changes(current: &str, changes: &[TextChange]) -> String {
    let mut text = current.to_owned();
    for change in changes {
        text = apply_one(&text, change);
    }
    text
}

fn apply_one(current: &str, change: &TextChange) -> String {
    let Some(range) = change.range else {
        return change.text.clone();
    };
    let map = PositionMap::new(current);
    let mut start = map.offset_of(current, range.start_line, range.start_character);
    let mut end = map.offset_of(current, range.end_line, range.end_character);
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let mut next = String::with_capacity(current.len() + change.text.len());
    next.push_str(&current[..start]);
    next.push_str(&change.text);
    next.push_str(&current[end..]);
    next
}

#[cfg(test)]
mod tests {
    use super::{apply_changes, TextChange, TextRange};

    fn ranged(
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
        text: &str,
    ) -> TextChange {
        TextChange {
            range: Some(TextRange {
                start_line,
                start_character,
                end_line,
                end_character,
            }),
            text: text.to_owned(),
        }
    }

    fn full(text: &str) -> TextChange {
        TextChange {
            range: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn full_replacement_wins() {
        assert_eq!(apply_changes("old\n", &[full("new\n")]), "new\n");
    }

    #[test]
    fn single_line_insertion() {
        let result = apply_changes("ab\n", &[ranged(0, 1, 0, 1, "X")]);
        assert_eq!(result, "aXb\n");
    }

    #[test]
    fn multiline_replace_collapses_lines() {
        let current = "line0\nline1\nline2\n";
        let result = apply_changes(current, &[ranged(0, 4, 1, 5, "NEW")]);
        assert_eq!(result, "lineNEW\nline2\n");
    }

    #[test]
    fn sequential_changes_compose() {
        let result = apply_changes("abc\n", &[ranged(0, 0, 0, 0, "1"), ranged(0, 1, 0, 1, "2")]);
        assert_eq!(result, "12abc\n");
    }

    #[test]
    fn utf16_columns_address_multibyte_text() {
        // 'é' is 1 UTF-16 unit but 2 UTF-8 bytes.
        let current = "aéb\n";
        let result = apply_changes(current, &[ranged(0, 1, 0, 2, "E")]);
        assert_eq!(result, "aEb\n");
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let result = apply_changes("ab\n", &[ranged(9, 99, 10, 100, "!")]);
        assert_eq!(result, "ab\n!");
    }

    #[test]
    fn reversed_range_is_swapped_not_rejected() {
        let result = apply_changes("abcdef\n", &[ranged(0, 4, 0, 2, "X")]);
        assert_eq!(result, "abXef\n");
    }

    /// Property: random edit scripts applied incrementally equal the same
    /// script folded over full-text snapshots. A tiny LCG keeps the test
    /// deterministic without new dependencies.
    #[test]
    fn random_scripts_match_clean_recomputation() {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        let alphabet = ["a", "é", "𝄞", "\n", "xy", " "];
        for _ in 0..200 {
            let mut text = String::new();
            for _ in 0..(next() % 24) {
                text.push_str(alphabet[next() % alphabet.len()]);
            }
            let map = crate::coords::PositionMap::new(&text);
            let line_count = map.line_count().max(1);
            let mut script = Vec::new();
            for _ in 0..(1 + next() % 4) {
                let line = (next() % (line_count as usize + 2)) as u32;
                let character = (next() % 8) as u32;
                let end_line = line + (next() % 2) as u32;
                script.push(ranged(
                    line,
                    character,
                    end_line,
                    character + (next() % 4) as u32,
                    alphabet[next() % alphabet.len()],
                ));
            }
            // Incremental path under test.
            let incremental = apply_changes(&text, &script);
            // Clean recomputation: re-derive offsets per step from scratch.
            let mut expected = text.clone();
            for change in &script {
                let range = change.range.expect("script uses ranges");
                let map = crate::coords::PositionMap::new(&expected);
                let mut start = map.offset_of(&expected, range.start_line, range.start_character);
                let mut end = map.offset_of(&expected, range.end_line, range.end_character);
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                expected = format!("{}{}{}", &expected[..start], change.text, &expected[end..]);
            }
            assert_eq!(incremental, expected, "script {script:?} on {text:?}");
            // No panics and valid UTF-8 are part of the assertion: both
            // strings are `String` by construction.
        }
    }
}
