//! A small, honest TextMate-subset tokenization engine.
//!
//! Purpose: exercise the MNCS grammar with real scoped-tokenization tests
//! without pulling a JavaScript engine into the test suite. The engine
//! implements exactly the constructs [`crate::grammar`] admits:
//!
//! - ordered per-position pattern matching within the active rule context;
//! - `begin`/`end` regions carried across lines via an explicit stack;
//! - capture groups applying scope names to sub-spans;
//! - repository includes, including region-recursive grammars.
//!
//! Zero-width matches are rejected at runtime (treated as non-matching) so a
//! pathological pattern cannot hang tokenization; validation rejects the
//! constructs that would otherwise make this matter.
//!
//! Scope paths accumulate outermost-first: enclosing region content scopes,
//! then the matched rule name, then capture names — mirroring TextMate
//! semantics closely enough for classification assertions.

use crate::grammar::{Captures, Grammar, GrammarError, RuleBody};

/// One emitted token on one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Byte offset of the token within its line (excluding the newline).
    pub start_byte: usize,
    /// Length of the token in bytes.
    pub length: usize,
    /// Full scope path, outermost first, deepest last.
    pub scopes: Vec<String>,
}

impl Token {
    /// The leaf (deepest) scope name, if any.
    pub fn leaf_scope(&self) -> Option<&str> {
        self.scopes.last().map(String::as_str)
    }

    /// Whether any scope on the path starts with `prefix`.
    pub fn has_scope_prefix(&self, prefix: &str) -> bool {
        self.scopes.iter().any(|scope| scope.starts_with(prefix))
    }
}

/// Safety bound against pathological grammars; unreachable for the MNCS
/// grammar because zero-width matches are ignored.
const MAX_REGION_DEPTH: usize = 10_000;

struct RegionFrame<'g> {
    end: &'g crate::grammar::Compiled,
    end_captures: &'g Captures,
    patterns: &'g [String],
    /// Scopes applied to everything inside the region: the region rule's
    /// own `name` followed by its `contentName` (TextMate semantics: only
    /// `contentName` applies to interior text, but storing both here lets
    /// the delimiter emission strip them cleanly).
    scope_prefixes: Vec<String>,
}

impl<'g> RegionFrame<'g> {
    fn new(
        name: Option<&str>,
        content_name: Option<&str>,
        end: &'g crate::grammar::Compiled,
        end_captures: &'g Captures,
        patterns: &'g [String],
    ) -> Self {
        let mut scope_prefixes = Vec::new();
        if let Some(name) = name {
            scope_prefixes.push(name.to_owned());
        }
        if let Some(content) = content_name {
            scope_prefixes.push(content.to_owned());
        }
        Self {
            end,
            end_captures,
            patterns,
            scope_prefixes,
        }
    }
}

/// Stateful highlighter; reuse across documents of the same grammar.
pub struct Highlighter<'g> {
    grammar: &'g Grammar,
    stack: Vec<RegionFrame<'g>>,
}

impl<'g> Highlighter<'g> {
    /// Create a highlighter starting from the grammar root.
    ///
    /// # Errors
    /// Propagates [`GrammarError`] if the grammar was not validated. Grammars
    /// obtained through [`crate::load_grammar`] are always validated.
    pub fn new(grammar: &'g Grammar) -> Result<Self, GrammarError> {
        Ok(Self {
            grammar,
            stack: Vec::new(),
        })
    }

    /// Tokenize a whole document, resetting state first.
    ///
    /// Returns one token vector per input line.
    pub fn tokenize(&mut self, text: &str) -> Vec<Vec<Token>> {
        self.stack.clear();
        split_lines(text)
            .into_iter()
            .map(|line| self.tokenize_line(line))
            .collect()
    }

    fn active_patterns(&self) -> &'g [String] {
        match self.stack.last() {
            Some(frame) => frame.patterns,
            None => self.grammar.root_patterns(),
        }
    }

    /// Enclosing content scopes, outermost first.
    fn base_scopes(&self) -> Vec<String> {
        self.stack
            .iter()
            .flat_map(|frame| frame.scope_prefixes.iter().cloned())
            .collect()
    }

    /// Base scopes excluding the innermost region (used when emitting that
    /// region's end delimiter, which carries the rule name itself).
    fn base_scopes_excluding_top(&self) -> Vec<String> {
        let mut scopes = Vec::new();
        for frame in self.stack.iter().rev().skip(1).rev() {
            scopes.extend(frame.scope_prefixes.iter().cloned());
        }
        scopes
    }

    fn tokenize_line(&mut self, line: &str) -> Vec<Token> {
        let bytes = line.as_bytes();
        let mut tokens = Vec::new();
        let mut pos = 0usize;
        // Start of the pending run of characters no rule matched. Such runs
        // are emitted with the enclosing region's content scopes (TextMate
        // `contentName` behavior); outside any region they stay unscoped.
        let mut run_start: Option<usize> = None;
        while pos < bytes.len() {
            if let Some(consumed) = self.try_region_end(line, pos, &mut tokens, &mut run_start) {
                pos += consumed;
                continue;
            }
            if let Some(consumed) = self.try_rules(line, pos, &mut tokens, &mut run_start) {
                pos += consumed;
                continue;
            }
            // No rule matched: hold this character in the pending run.
            run_start.get_or_insert(pos);
            pos += next_char_len(bytes, pos);
        }
        self.flush_run(&mut tokens, &mut run_start, bytes.len());
        tokens
    }

    /// Emit a pending unmatched-character run with enclosing content scopes.
    fn flush_run(&mut self, tokens: &mut Vec<Token>, run_start: &mut Option<usize>, upto: usize) {
        if let Some(start) = run_start.take() {
            if upto > start && !self.stack.is_empty() {
                tokens.push(Token {
                    start_byte: start,
                    length: upto - start,
                    scopes: self.base_scopes(),
                });
            }
        }
    }

    /// Try the enclosing region's end pattern at `pos`; on success emit its
    /// tokens and pop the region.
    fn try_region_end(
        &mut self,
        line: &str,
        pos: usize,
        tokens: &mut Vec<Token>,
        run_start: &mut Option<usize>,
    ) -> Option<usize> {
        // Copy the grammar-bound references out so the stack can mutate.
        let (end, end_captures, region_name, base) = {
            let frame = self.stack.last()?;
            (
                frame.end,
                frame.end_captures,
                frame.scope_prefixes.first().cloned(),
                self.base_scopes_excluding_top(),
            )
        };
        let spans = end.captures_at(line, pos)?;
        let (start, stop) = spans.first().copied().flatten()?;
        // Flush any pending run while this region is still on the stack so
        // the text inherits its content scopes.
        self.flush_run(tokens, run_start, pos);
        emit_match(
            tokens,
            SpanMatch { line, start, stop },
            &base,
            region_name.as_deref(),
            end_captures,
            &spans,
        );
        self.stack.pop();
        Some(stop - start)
    }

    /// Try each rule of the active context, in order, anchored at `pos`.
    fn try_rules(
        &mut self,
        line: &str,
        pos: usize,
        tokens: &mut Vec<Token>,
        run_start: &mut Option<usize>,
    ) -> Option<usize> {
        let patterns = self.active_patterns();
        for name in patterns {
            let rule = self.grammar.rule(name)?;
            match &rule.body {
                // Groups are flattened away at load time; a surviving group
                // here would be a loader bug, so it is skipped.
                RuleBody::Group { .. } => continue,
                RuleBody::Match { regex, captures } => {
                    let Some(spans) = regex.captures_at(line, pos) else {
                        continue;
                    };
                    let Some((start, stop)) = spans.first().copied().flatten() else {
                        continue;
                    };
                    if stop == start {
                        continue; // ignore zero-width matches
                    }
                    self.flush_run(tokens, run_start, pos);
                    let base = self.base_scopes();
                    emit_match(
                        tokens,
                        SpanMatch { line, start, stop },
                        &base,
                        rule.name.as_deref(),
                        captures,
                        &spans,
                    );
                    return Some(stop - start);
                }
                RuleBody::BeginEnd(data) => {
                    let (begin, begin_captures) = (&data.begin, &data.begin_captures);
                    let Some(spans) = begin.captures_at(line, pos) else {
                        continue;
                    };
                    let Some((start, stop)) = spans.first().copied().flatten() else {
                        continue;
                    };
                    if stop == start || self.stack.len() >= MAX_REGION_DEPTH {
                        continue;
                    }
                    self.flush_run(tokens, run_start, pos);
                    let base = self.base_scopes();
                    emit_match(
                        tokens,
                        SpanMatch { line, start, stop },
                        &base,
                        rule.name.as_deref(),
                        begin_captures,
                        &spans,
                    );
                    self.stack.push(RegionFrame::new(
                        rule.name.as_deref(),
                        data.content_name.as_deref(),
                        &data.end,
                        &data.end_captures,
                        &data.patterns,
                    ));
                    return Some(stop - start);
                }
            }
        }
        None
    }
}

/// A matched span together with the line it was found on.
struct SpanMatch<'l> {
    line: &'l str,
    start: usize,
    stop: usize,
}

fn emit_match(
    tokens: &mut Vec<Token>,
    span: SpanMatch<'_>,
    base: &[String],
    rule_name: Option<&str>,
    captures: &Captures,
    spans: &[Option<(usize, usize)>],
) {
    let (line, start, stop) = (span.line, span.start, span.stop);
    if let Some(rule_name) = rule_name {
        let mut scopes = base.to_vec();
        scopes.push(rule_name.to_owned());
        tokens.push(Token {
            start_byte: start,
            length: stop - start,
            scopes,
        });
    }
    for (index, name) in captures {
        let Some(name) = name else {
            continue;
        };
        let Some(Some((group_start, group_stop))) = spans.get(*index).copied() else {
            continue;
        };
        if group_start >= group_stop
            || !line.is_char_boundary(group_start)
            || !line.is_char_boundary(group_stop)
        {
            continue;
        }
        let mut scopes = base.to_vec();
        if let Some(rule_name) = rule_name {
            scopes.push(rule_name.to_owned());
        }
        scopes.push(name.clone());
        tokens.push(Token {
            start_byte: group_start,
            length: group_stop - group_start,
            scopes,
        });
    }
}
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&text[start..index]);
            start = index + 1;
        }
    }
    if start < text.len() || text.is_empty() {
        lines.push(&text[start..]);
    }
    lines
}

fn next_char_len(bytes: &[u8], pos: usize) -> usize {
    let lead = bytes[pos];
    if lead < 0x80 {
        1
    } else {
        std::str::from_utf8(&bytes[pos..])
            .ok()
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8)
    }
}
