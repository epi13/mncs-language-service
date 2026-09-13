//! Deterministic document formatting.
//!
//! The formatter is presentation-only: it adjusts leading indentation,
//! trailing whitespace, blank-line runs, and the final newline. It never
//! moves, adds, or deletes a significant token, so formatting cannot change
//! program meaning. Idempotence and token preservation are tested as
//! properties, not assumed.
//!
//! Indentation is brace- and bracket-depth driven (4 spaces per level) with
//! comment-aware scanning: braces inside `//` and `/* */` comments never
//! affect depth, and comment-interior lines keep their relative shape only
//! through reindentation of the line start.

use serde::{Deserialize, Serialize};

use crate::queries::{snapshot_info, ResponseStatus, ServiceError, SnapshotInfo};
use crate::rename::{FileEdit, SingleEdit};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormattingResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    /// The full formatted text.
    pub text: String,
    /// Whether the input was already canonical.
    pub already_formatted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeFormattingResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    /// Line-scoped edits (whole-line replacements) covering only the
    /// requested range. Applying them equals formatting that range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FileEdit>,
    pub already_formatted: bool,
}

use crate::queries::LanguageService;

const INDENT_WIDTH: usize = 4;

impl LanguageService {
    /// Format the whole document deterministically.
    pub fn formatting(&self, uri: &str) -> Result<FormattingResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let formatted = format_text(snapshot.text());
        Ok(FormattingResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            already_formatted: formatted == snapshot.text(),
            text: formatted,
        })
    }

    /// Format only lines `start_line..=end_line`: the full-document format is
    /// computed (formatting is global by nature — depth crosses the range),
    /// then restricted to whole-line replacements inside the range so
    /// outside lines are untouched byte-for-byte.
    pub fn range_formatting(
        &self,
        uri: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<RangeFormattingResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let current = snapshot.text();
        let formatted = format_text(current);
        if formatted == current {
            return Ok(RangeFormattingResponse {
                status: ResponseStatus::Answered,
                snapshot: Some(info()),
                changes: Vec::new(),
                already_formatted: true,
            });
        }
        let (start_line, end_line) = if start_line <= end_line {
            (start_line, end_line)
        } else {
            (end_line, start_line)
        };
        let current_lines: Vec<&str> = current.split('\n').collect();
        let formatted_lines: Vec<&str> = formatted.split('\n').collect();
        if current_lines.len() != formatted_lines.len() {
            // The formatter never adds or removes newlines except collapsing
            // blank runs and the final newline; a line-count change means the
            // range cannot be expressed as whole-line edits, so refuse rather
            // than reformat outside the range.
            return Ok(RangeFormattingResponse {
                status: ResponseStatus::Unsupported {
                    reason: "the formatted output changes line breaks outside a whole-line mapping; use document formatting".to_owned(),
                },
                snapshot: Some(info()),
                changes: Vec::new(),
                already_formatted: false,
            });
        }
        let map = crate::coords::PositionMap::new(current);
        let mut edits = Vec::new();
        for (index, (old, new)) in current_lines.iter().zip(formatted_lines.iter()).enumerate() {
            let line = index as u32;
            if line < start_line || line > end_line || old == new {
                continue;
            }
            let start_byte = map.offset_of(current, line, 0);
            let end_byte = start_byte + old.len();
            edits.push(SingleEdit {
                range: map.range_of(
                    current,
                    mncs_syntax::SourceSpan::at(current, start_byte, end_byte),
                ),
                new_text: (*new).to_owned(),
            });
        }
        let already_formatted = edits.is_empty();
        Ok(RangeFormattingResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            changes: if already_formatted {
                Vec::new()
            } else {
                vec![FileEdit {
                    uri: uri.to_owned(),
                    edits,
                }]
            },
            already_formatted,
        })
    }
}

/// Format `text` deterministically: reindent by nesting depth, trim trailing
/// whitespace, collapse blank-line runs to one, ensure exactly one trailing
/// newline. Line-ending style (LF vs CRLF) is preserved.
pub fn format_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let crlf = text.contains("\r\n");
    let stripped = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = stripped.split('\n').map(str::to_owned).collect();
    // `split` yields a trailing empty element for a final newline; drop it
    // and re-add exactly one later.
    let had_trailing = lines.last().is_some_and(|line| line.is_empty());
    if had_trailing {
        lines.pop();
    }

    let mut depth: usize = 0;
    let mut in_block_comment = false;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines {
        let trimmed_end = trim_trailing(line);
        if trimmed_end.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let (leading_closes, net, block_end) = scan_line(trimmed_end, in_block_comment);
        in_block_comment = block_end;
        let mut indent = depth.saturating_sub(leading_closes);
        // Contract/capability/effect clauses continue a function signature
        // header on their own lines, indented one level in canonical sources
        // (`fn f(...)` / `    requires ...` / `{`). They never open a block
        // themselves, so depth is unaffected.
        if is_clause_continuation(trimmed_end) {
            indent += 1;
        }
        let content = trimmed_end.trim_start();
        out.push(format!("{}{}", " ".repeat(indent * INDENT_WIDTH), content));
        // `net` already includes the leading closes, so one update suffices:
        // indent with depth-before, continue with depth-after.
        depth = (depth as i64 + net).max(0) as usize;
    }
    // Collapse blank runs to a single blank line.
    let mut collapsed: Vec<String> = Vec::with_capacity(out.len());
    let mut blanks = 0usize;
    for line in out {
        if line.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                collapsed.push(line);
            }
        } else {
            blanks = 0;
            collapsed.push(line);
        }
    }
    // Drop leading blank lines; exactly one trailing newline.
    while collapsed.first().is_some_and(|line| line.is_empty()) {
        collapsed.remove(0);
    }
    while collapsed.last().is_some_and(|line| line.is_empty()) {
        collapsed.pop();
    }
    let mut result = collapsed.join("\n");
    result.push('\n');
    if crlf {
        result.replace('\n', "\r\n")
    } else {
        result
    }
}

fn trim_trailing(line: &str) -> &str {
    line.trim_end_matches([' ', '\t'])
}

/// Whether a line continues a function signature header with a
/// contract/capability/effect clause. These keywords never start a body
/// statement, so a line leading with one is a header continuation.
fn is_clause_continuation(line: &str) -> bool {
    let first = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(':');
    matches!(
        first,
        "requires"
            | "ensures"
            | "assumes"
            | "property"
            | "invariant"
            | "metamorphic"
            | "effect"
            | "capability"
            | "authorized_by"
    )
}

/// Scan one line outside string-free MNCS source: count leading close
/// brackets (for dedent-before-indent), the net depth delta, and whether a
/// block comment remains open at end of line.
fn scan_line(line: &str, mut in_block: bool) -> (usize, i64, bool) {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut seen_code = false;
    let mut leading_closes = 0usize;
    let mut net: i64 = 0;
    while index < bytes.len() {
        if in_block {
            if line[index..].starts_with("*/") {
                in_block = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if line[index..].starts_with("//") {
            break;
        }
        if line[index..].starts_with("/*") {
            in_block = true;
            index += 2;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' => {
                if !seen_code {
                    seen_code = true;
                }
                net += 1;
                index += 1;
            }
            b'}' | b']' => {
                if !seen_code {
                    leading_closes += 1;
                } else {
                    // A close after code on the same line (e.g. `} else {`)
                    // still dedents the *next* line via the net delta only
                    // once the line's own opens are accounted for below.
                }
                net -= 1;
                seen_code = true;
                index += 1;
            }
            b' ' | b'\t' => {
                index += 1;
            }
            _ => {
                seen_code = true;
                index += 1;
            }
        }
    }
    // `leading_closes` counted closes before any code; the net delta already
    // includes them, so the caller dedents first and then applies the net.
    (leading_closes.min(8), net, in_block)
}

#[cfg(test)]
mod tests {
    use super::format_text;

    fn significant_tokens(text: &str) -> Vec<(String, String)> {
        // Token preservation oracle that does not depend on spans: the exact
        // sequence of (kind, text) pairs must survive formatting.
        let envelope = mncs_syntax::SourceEnvelope::new(
            mncs_syntax::SourceArtifactKind::Program,
            "test://format".to_owned(),
            mncs_syntax::SourceOrigin {
                kind: mncs_syntax::SourceOriginKind::Inline,
                locator: None,
            },
            text.to_owned(),
        );
        let lexed = mncs_syntax::lex(&envelope);
        lexed
            .tokens
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| (format!("{:?}", token.kind), token.text))
            .collect()
    }

    #[test]
    fn already_canonical_text_is_untouched() {
        let text = "mncs 0.3;\n\nmodule examples.contracts;\n\nfn f(n: i64) -> (result: i64)\n{\n    return n;\n}\n";
        assert_eq!(format_text(text), text);
    }

    #[test]
    fn indentation_trailing_space_and_blank_runs_normalize() {
        let text =
            "mncs 0.3;\n\n\nmodule m;\nfn f(n: i64) -> (result: i64)\n{\n        return n;   \n}\n";
        let expected =
            "mncs 0.3;\n\nmodule m;\nfn f(n: i64) -> (result: i64)\n{\n    return n;\n}\n";
        assert_eq!(format_text(text), expected);
    }

    #[test]
    fn braces_in_comments_do_not_change_depth() {
        let text = "fn f(n: i64) -> (result: i64)\n{\n    // open { brace in comment\n    return n;\n    /* block } brace */\n}\n";
        assert_eq!(format_text(text), text);
    }

    #[test]
    fn formatting_is_idempotent() {
        let cases = [
            "mncs 0.3;\nmodule m;\nfn f(n: i64) -> (result: i64)\n{\nif n > 1 {\nreturn n;\n}\nreturn 0;\n}\n",
            "mncs 0.5;\nmodule m;\nrecord R { x: i32 }\nfn f(r: R) -> (v: i32)\n{\nreturn r.x;\n}\n",
            "fn f(n: i64) -> (result: i64)\n{\n    /* multi\n       line { comment\n       still comment */\n    return n;\n}\n",
            "\n\n\nfn f(n: i64) -> (result: i64)\n{\nreturn n;\n}\n\n\n",
            "",
        ];
        for text in cases {
            let once = format_text(text);
            assert_eq!(format_text(&once), once, "not idempotent for {text:?}");
        }
    }

    #[test]
    fn formatting_preserves_the_significant_token_stream() {
        let texts = [
            "mncs 0.3;\n\nmodule examples.contracts;\n\nfn bounded_step(n: i64, limit: i64) -> (result: i64)\n    requires n_within_limit\n    capability checked_integer\n{\n    if n > limit {\n        fail isolated;\n    }\n    let next: i64 = n + 1;\n    return next;\n}\n",
            "mncs 0.5;\nmodule m;\nrecord Reading { celsius: i32 }\nfn adjust(celsius: i32) -> (result: i32)\n{\n    let base: Reading = Reading { celsius: celsius };\n    return base.celsius;\n}\n",
        ];
        for text in texts {
            let formatted = format_text(text);
            assert_eq!(
                significant_tokens(&formatted),
                significant_tokens(text),
                "token stream changed for {text:?}"
            );
        }
    }
}
