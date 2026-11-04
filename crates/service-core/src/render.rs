//! Shared rendering: hover markdown, semantic token classification, and
//! conservative completion.
//!
//! Rendering lives in the core so every adapter presents identical semantic
//! content; adapters only re-encode presentation for their protocol.

use std::collections::BTreeSet;

use mncs_syntax::{AbstractSyntaxTree, AstFunction, AstStmt, TokenKind};

use crate::analysis::DocumentAnalysis;
use crate::indexes::{self, SymbolKind};
use crate::queries::{CompletionCandidate, CompletionClass, TokenAnnotation, TokenClass};

pub(crate) const KEYWORDS: [&str; 30] = [
    "mncs",
    "module",
    "fn",
    "return",
    "let",
    "if",
    "else",
    "fail",
    "requires",
    "ensures",
    "assumes",
    "property",
    "invariant",
    "metamorphic",
    "effect",
    "capability",
    "authorized_by",
    "enum",
    "use",
    "record",
    "match",
    "iterate",
    "up_to",
    "carrying",
    "next",
    "over",
    "as",
    "while",
    "true",
    "false",
];

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

pub(crate) fn render_hover_markdown(snapshot: &DocumentAnalysis, target: usize) -> String {
    let entry = &snapshot.symbols.symbols[target];
    let mut sections: Vec<String> = Vec::new();

    sections.push(signature_section(snapshot, entry));

    if let Some(ast) = snapshot.front_end.ast.as_ref() {
        match entry.kind {
            SymbolKind::FiniteType => {
                if let Some(declared) = ast
                    .finite_types
                    .iter()
                    .find(|candidate| candidate.name.text == entry.name)
                {
                    sections.push(format!(
                        "```mncs\nenum {} {{ {} }}\n```",
                        declared.name.text,
                        declared
                            .variants
                            .iter()
                            .map(|variant| variant.name.text.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            SymbolKind::RecordType => {
                if let Some(declared) = ast
                    .record_types
                    .iter()
                    .find(|candidate| candidate.name.text == entry.name)
                {
                    sections.push(format!(
                        "```mncs\nrecord {} {{ {} }}\n```\n*logical record identity orders fields by name; declaration order is not part of the type*",
                        declared.name.text,
                        declared
                            .fields
                            .iter()
                            .map(|field| format!("{}: {}", field.name.text, field.value_type.text))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            SymbolKind::FiniteVariant => {
                if let Some(parent) = &entry.container {
                    sections.push(format!("variant of finite type `{parent}`"));
                }
            }
            SymbolKind::RecordField => {
                if let (Some(parent), Some(ty)) = (&entry.container, &entry.type_name) {
                    sections.push(format!(
                        "field of record `{parent}`: `{}`: {ty}",
                        entry.name
                    ));
                }
            }
            _ => {}
        }
    }

    // Function-level semantic context from the elaborated program.
    if let Some(program) = snapshot.front_end.program.as_ref() {
        if let Some(function) = program
            .functions
            .iter()
            .find(|candidate| candidate.name == entry.name)
        {
            if !function.contracts.is_empty() {
                let lines = function
                    .contracts
                    .iter()
                    .map(|clause| {
                        format!(
                            "- `{}` {} — `{}`",
                            contract_kind(&clause.kind),
                            clause.id,
                            clause.expression
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                sections.push(format!("**Contracts**\n{lines}"));
            }
            if !function.capabilities.is_empty() {
                sections.push(format!(
                    "**Capabilities**: {}",
                    function.capabilities.join(", ")
                ));
            }
            if !function.effects.is_empty() {
                let effects = function
                    .effects
                    .iter()
                    .map(|effect| format!("{} (authorized_by {})", effect.kind, effect.capability))
                    .collect::<Vec<_>>()
                    .join(", ");
                sections.push(format!("**Effects**: {effects}"));
            }
            if let Some(identity) = &entry.identity {
                let generation = program.generate_obligations();
                let relevant: Vec<_> = generation
                    .obligations
                    .iter()
                    .filter(|obligation| {
                        obligation.subject == *identity
                            || obligation
                                .dependencies
                                .iter()
                                .any(|dependency| dependency == identity)
                    })
                    .collect();
                if !relevant.is_empty() {
                    let count = |wanted: mncs_model::ObligationStatus| {
                        relevant
                            .iter()
                            .filter(|obligation| obligation.status == wanted)
                            .count()
                    };
                    sections.push(format!(
                        "**Obligations**: {} pass, {} fail, {} unknown",
                        count(mncs_model::ObligationStatus::Pass),
                        count(mncs_model::ObligationStatus::Fail),
                        count(mncs_model::ObligationStatus::Unknown)
                    ));
                }
            }
        }
    }

    if let Some(identity) = &entry.identity {
        sections.push(format!("*identity*: `{identity}`"));
    }

    sections.join("\n\n")
}

fn signature_section(snapshot: &DocumentAnalysis, entry: &indexes::SymbolEntry) -> String {
    match entry.kind {
        SymbolKind::Module => format!("```mncs\nmodule {};\n```", entry.name),
        SymbolKind::Function => {
            if let Some(ast) = snapshot.front_end.ast.as_ref() {
                if let Some(function) = ast
                    .functions
                    .iter()
                    .find(|candidate| candidate.name.text == entry.name)
                {
                    return format!(
                        "```mncs\nfn {}({}) -> ({})\n```",
                        function.name.text,
                        parameters_text(&function.inputs),
                        parameters_text(&function.outputs),
                    );
                }
            }
            format!("```mncs\nfn {}\n```", entry.name)
        }
        SymbolKind::Parameter | SymbolKind::Binding | SymbolKind::IterationState => {
            let role = match entry.kind {
                SymbolKind::Parameter => "param",
                SymbolKind::Binding => "let",
                _ => "carried state",
            };
            format!(
                "```mncs\n{} {}: {}\n```",
                role,
                entry.name,
                entry.type_name.clone().unwrap_or_else(|| "?".to_owned())
            )
        }
        SymbolKind::FiniteType => format!("```mncs\nenum {}\n```", entry.name),
        SymbolKind::RecordType => format!("```mncs\nrecord {}\n```", entry.name),
        SymbolKind::FiniteVariant => format!("`{}` (finite variant)", entry.name),
        SymbolKind::RecordField => format!(
            "`{}`: {}",
            entry.name,
            entry.type_name.clone().unwrap_or_else(|| "?".to_owned())
        ),
    }
}

fn parameters_text(parameters: &[mncs_syntax::AstParameter]) -> String {
    parameters
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name.text, parameter.value_type.text))
        .collect::<Vec<_>>()
        .join(", ")
}

fn contract_kind(kind: &mncs_model::ContractKind) -> &'static str {
    match kind {
        mncs_model::ContractKind::Requires => "requires",
        mncs_model::ContractKind::Ensures => "ensures",
        mncs_model::ContractKind::Invariant => "invariant",
        mncs_model::ContractKind::Preserves => "preserves",
        mncs_model::ContractKind::Budget => "budget",
        mncs_model::ContractKind::Property => "property",
        mncs_model::ContractKind::Metamorphic => "metamorphic",
    }
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

pub(crate) fn compute_semantic_tokens(snapshot: &DocumentAnalysis) -> Vec<TokenAnnotation> {
    let text = snapshot.text();
    let mut annotations = Vec::new();

    let significant: Vec<&mncs_syntax::SourceToken> = snapshot
        .front_end
        .lexical
        .tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();

    for (position, token) in significant.iter().enumerate() {
        let class = match token.kind {
            TokenKind::IntegerLiteral | TokenKind::Version => Some(TokenClass::Number),
            kind if is_keyword_token(kind) => Some(TokenClass::Keyword),
            TokenKind::Identifier => classify_identifier(snapshot, position, &significant, token),
            _ => None,
        };
        let Some(class) = class else { continue };
        let start = snapshot.positions.position_of(text, token.span.start);
        let length_utf16: u32 = token.text.chars().map(char::len_utf16).sum::<usize>() as u32;
        annotations.push(TokenAnnotation {
            start_line: start.line,
            start_character: start.character,
            length_utf16,
            class,
        });
    }
    annotations
}

fn is_keyword_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::MncsKeyword
            | TokenKind::ModuleKeyword
            | TokenKind::FunctionKeyword
            | TokenKind::ReturnKeyword
            | TokenKind::LetKeyword
            | TokenKind::IfKeyword
            | TokenKind::ElseKeyword
            | TokenKind::FailKeyword
            | TokenKind::RequiresKeyword
            | TokenKind::EnsuresKeyword
            | TokenKind::AssumesKeyword
            | TokenKind::PropertyKeyword
            | TokenKind::InvariantKeyword
            | TokenKind::MetamorphicKeyword
            | TokenKind::EffectKeyword
            | TokenKind::CapabilityKeyword
            | TokenKind::AuthorizedKeyword
            | TokenKind::EnumKeyword
            | TokenKind::UseKeyword
            | TokenKind::RecordKeyword
            | TokenKind::MatchKeyword
            | TokenKind::IterateKeyword
            | TokenKind::UpToKeyword
            | TokenKind::CarryingKeyword
            | TokenKind::NextKeyword
            | TokenKind::OverKeyword
            | TokenKind::AsKeyword
            | TokenKind::WhileKeyword
            | TokenKind::TrueKeyword
            | TokenKind::FalseKeyword
    )
}

fn classify_identifier(
    snapshot: &DocumentAnalysis,
    position: usize,
    significant: &[&mncs_syntax::SourceToken],
    token: &mncs_syntax::SourceToken,
) -> Option<TokenClass> {
    let byte = token.span.start;
    let resolved_kind = snapshot
        .symbols
        .references_at(byte)
        .next()
        .map(|reference| reference.kind)
        .or_else(|| {
            snapshot
                .symbols
                .declaration_at(byte)
                .map(|index| snapshot.symbols.symbols[index].kind)
        });
    if let Some(kind) = resolved_kind {
        return Some(match kind {
            SymbolKind::Module => TokenClass::Module,
            SymbolKind::Function => TokenClass::Function,
            SymbolKind::Parameter => TokenClass::Parameter,
            SymbolKind::Binding | SymbolKind::IterationState => TokenClass::Variable,
            SymbolKind::FiniteType | SymbolKind::RecordType => TokenClass::Type,
            SymbolKind::FiniteVariant => TokenClass::Variant,
            SymbolKind::RecordField => TokenClass::Field,
        });
    }
    // Conservative syntactic fallbacks only:
    // 1. identifier directly after `module` names the module;
    // 2. identifier followed by ':' that names a builtin scalar type is a
    //    type annotation.
    if position > 0 && significant[position - 1].kind == TokenKind::ModuleKeyword {
        return Some(TokenClass::Module);
    }
    if let Some(next) = significant.get(position + 1) {
        if next.kind == TokenKind::Colon && indexes::is_builtin_type(&token.text) {
            return Some(TokenClass::Type);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

pub(crate) fn compute_completion(
    uri: &str,
    snapshot: &DocumentAnalysis,
    line: u32,
    character: u32,
    native_prefix_matches: &mut dyn FnMut(&[String], &str) -> Option<Vec<bool>>,
) -> Vec<CompletionCandidate> {
    let text = snapshot.text();
    let byte = snapshot.positions.offset_of(text, line, character);

    // Never complete inside comments: low-confidence by definition.
    let inside_comment = snapshot.front_end.lexical.tokens.iter().any(|token| {
        matches!(token.kind, TokenKind::LineComment | TokenKind::BlockComment)
            && token.span.start <= byte
            && byte <= token.span.end
    });
    if inside_comment {
        return Vec::new();
    }

    let significant: Vec<&mncs_syntax::SourceToken> = snapshot
        .front_end
        .lexical
        .tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();

    // Nearest token at or left of the cursor. The end bound is forgiving by
    // one so a cursor resting just past a word still finds it.
    let cursor_index = significant
        .iter()
        .rposition(|token| token.span.start <= byte && byte <= token.span.end + 1);

    // A partially typed word: identifiers *and* keywords participate.
    let mut prefix = String::new();
    let mut member_context = false;
    if let Some(index) = cursor_index {
        let token = significant[index];
        let word_like = matches!(token.kind, TokenKind::Identifier) || is_keyword_token(token.kind);
        if byte > token.span.start && word_like {
            let within = (byte - token.span.start).min(token.text.len());
            prefix = token.text[..within].to_owned();
        }
        if token.kind == TokenKind::Dot {
            member_context = true;
        }
        if index > 0 && significant[index - 1].kind == TokenKind::Dot {
            member_context = true;
        }
    }

    // On a `use` line, complete module paths from the discovered stdlib
    // manifest instead of document symbols. This precedes member-context
    // detection: the dots in a module path are separators, not member
    // access.
    if let Some(module_prefix) = use_line_prefix(text, line, character) {
        return module_completions(&module_prefix, native_prefix_matches);
    }

    if member_context {
        return member_completions(uri, snapshot, cursor_index, &significant);
    }

    scoped_candidates(
        uri,
        snapshot,
        byte,
        &prefix,
        !prefix.is_empty(),
        native_prefix_matches,
    )
}

/// The partially typed module path when the cursor sits on a `use` line
/// after the `use` keyword, else `None`.
fn use_line_prefix(text: &str, line: u32, character: u32) -> Option<String> {
    let line_text = text.lines().nth(line as usize)?;
    let trimmed = line_text.trim_start();
    if !trimmed.starts_with("use ") {
        return None;
    }
    let indent = line_text.len() - trimmed.len();
    let cursor = character as usize;
    if cursor <= indent + 4 {
        return Some(String::new());
    }
    let after_use = &line_text[indent + 4..cursor.min(line_text.len())];
    if after_use.contains(';') {
        return None;
    }
    Some(
        after_use
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.' || *c == '_')
            .collect(),
    )
}

/// Module-path candidates from the discovered stdlib manifest,
/// prefix-filtered. Best-effort: no manifest, no candidates.
fn module_completions(
    prefix: &str,
    native_prefix_matches: &mut dyn FnMut(&[String], &str) -> Option<Vec<bool>>,
) -> Vec<CompletionCandidate> {
    let mut collector =
        CompletionCollector::new(prefix, !prefix.is_empty(), false, native_prefix_matches);
    for (name, profile) in crate::modules::stdlib_manifest_modules() {
        collector.consider(CompletionCandidate {
            label: name,
            class: CompletionClass::Module,
            detail: Some(format!("stdlib (profile {profile})")),
        });
    }
    collector.finish(false)
}

/// Collect completion results while keeping prefix work and temporary memory
/// bounded to one MNCS batch. The Rust predicate remains the fallback for
/// labels outside the byte-view envelope or when native execution is absent.
struct CompletionCollector<'a> {
    prefix: &'a str,
    filter_by_prefix: bool,
    deduplicate: bool,
    seen: BTreeSet<String>,
    pending: Vec<CompletionCandidate>,
    items: Vec<CompletionCandidate>,
    native_prefix_matches: &'a mut dyn FnMut(&[String], &str) -> Option<Vec<bool>>,
}

impl<'a> CompletionCollector<'a> {
    fn new(
        prefix: &'a str,
        filter_by_prefix: bool,
        deduplicate: bool,
        native_prefix_matches: &'a mut dyn FnMut(&[String], &str) -> Option<Vec<bool>>,
    ) -> Self {
        Self {
            prefix,
            filter_by_prefix,
            deduplicate,
            seen: BTreeSet::new(),
            pending: Vec::new(),
            items: Vec::new(),
            native_prefix_matches,
        }
    }

    fn consider(&mut self, candidate: CompletionCandidate) {
        if self.filter_by_prefix {
            self.pending.push(candidate);
            if self.pending.len() == crate::native_filter::MAX_TEXT_MATCH_ITEMS {
                self.flush_prefix_batch();
            }
        } else {
            self.push(candidate);
        }
    }

    fn flush_prefix_batch(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.pending);
        let native_mask = if self.prefix.len() <= crate::native_filter::MAX_TEXT_MATCH_BYTES
            && batch.iter().all(|candidate| {
                candidate.label.len() <= crate::native_filter::MAX_TEXT_MATCH_BYTES
            }) {
            let labels: Vec<String> = batch
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect();
            (self.native_prefix_matches)(&labels, self.prefix)
                .filter(|mask| mask.len() == batch.len())
        } else {
            None
        };

        for (index, candidate) in batch.into_iter().enumerate() {
            let matches = native_mask
                .as_ref()
                .map(|mask| mask[index])
                .unwrap_or_else(|| candidate.label.starts_with(self.prefix));
            if matches {
                self.push(candidate);
            }
        }
    }

    fn push(&mut self, candidate: CompletionCandidate) {
        if !self.deduplicate || self.seen.insert(candidate.label.clone()) {
            self.items.push(candidate);
        }
    }

    fn finish(mut self, sort_and_limit: bool) -> Vec<CompletionCandidate> {
        if self.filter_by_prefix {
            self.flush_prefix_batch();
        }
        if sort_and_limit {
            self.items
                .sort_by(|left, right| left.label.cmp(&right.label));
            self.items.truncate(MAX_COMPLETION_ITEMS);
        }
        self.items
    }
}

fn member_completions(
    uri: &str,
    snapshot: &DocumentAnalysis,
    cursor_index: Option<usize>,
    significant: &[&mncs_syntax::SourceToken],
) -> Vec<CompletionCandidate> {
    // Locate the dot left of the cursor, then the base identifier before it.
    let mut dot_position = None;
    if let Some(index) = cursor_index {
        if significant[index].kind == TokenKind::Dot {
            dot_position = Some(index);
        } else if index > 0 && significant[index - 1].kind == TokenKind::Dot {
            dot_position = Some(index - 1);
        }
    }
    let Some(dot_position) = dot_position else {
        return Vec::new();
    };
    let Some(base_token) = dot_position
        .checked_sub(1)
        .and_then(|base_index| significant.get(base_index))
        .copied()
        .cloned()
    else {
        return Vec::new();
    };
    if base_token.kind != TokenKind::Identifier {
        return Vec::new();
    }

    // Resolve the base identifier to its declaration.
    if let Some(reference) = snapshot.symbols.references_at(base_token.span.start).next() {
        let Some(target) = reference.target else {
            return Vec::new();
        };
        let entry = &snapshot.symbols.symbols[target];
        return match entry.kind {
            SymbolKind::FiniteType => variants_of(uri, snapshot, target),
            SymbolKind::Parameter | SymbolKind::Binding | SymbolKind::IterationState => {
                let declared_type = entry.type_name.as_deref();
                snapshot
                    .symbols
                    .symbols
                    .iter()
                    .position(|candidate| {
                        candidate.kind == SymbolKind::RecordType
                            && Some(candidate.name.as_str()) == declared_type
                    })
                    .map(|record_index| fields_of(uri, snapshot, record_index))
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
    }
    // Bare nominal type name used as constructor namespace.
    if let Some(index) = snapshot.symbols.symbols.iter().position(|entry| {
        entry.kind == SymbolKind::FiniteType
            && entry.name_span.start <= base_token.span.start
            && base_token.span.end <= entry.name_span.end
    }) {
        return variants_of(uri, snapshot, index);
    }
    Vec::new()
}

fn variants_of(
    uri: &str,
    snapshot: &DocumentAnalysis,
    type_index: usize,
) -> Vec<CompletionCandidate> {
    snapshot
        .symbols
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.parent == Some(type_index) && entry.kind == SymbolKind::FiniteVariant
        })
        .map(|(index, _)| candidate_from(uri, snapshot, index))
        .collect()
}

fn fields_of(
    uri: &str,
    snapshot: &DocumentAnalysis,
    record_index: usize,
) -> Vec<CompletionCandidate> {
    snapshot
        .symbols
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.parent == Some(record_index) && entry.kind == SymbolKind::RecordField
        })
        .map(|(index, _)| candidate_from(uri, snapshot, index))
        .collect()
}

fn candidate_from(uri: &str, snapshot: &DocumentAnalysis, index: usize) -> CompletionCandidate {
    let summary = crate::queries::summarize_public(uri, snapshot, index);
    CompletionCandidate {
        label: summary.name,
        class: CompletionClass::Symbol(summary.kind),
        detail: summary.detail,
    }
}

fn scoped_candidates(
    uri: &str,
    snapshot: &DocumentAnalysis,
    byte: usize,
    prefix: &str,
    filter_by_prefix: bool,
    native_prefix_matches: &mut dyn FnMut(&[String], &str) -> Option<Vec<bool>>,
) -> Vec<CompletionCandidate> {
    let mut collector =
        CompletionCollector::new(prefix, filter_by_prefix, true, native_prefix_matches);

    // Visible locals along the elaboration path to the cursor.
    if let Some(ast) = snapshot.front_end.ast.as_ref() {
        if let Some(function) = function_at(ast, byte) {
            for parameter in &function.inputs {
                collector.consider(CompletionCandidate {
                    label: parameter.name.text.clone(),
                    class: CompletionClass::Symbol(SymbolKind::Parameter),
                    detail: Some(format!(
                        "{}: {}",
                        parameter.name.text, parameter.value_type.text
                    )),
                });
            }
            let mut visible = Vec::new();
            collect_visible_locals(&function.body.statements, byte, &mut visible);
            for (name, _type_name) in visible {
                collector.consider(CompletionCandidate {
                    label: name,
                    class: CompletionClass::Variable,
                    detail: None,
                });
            }
        }
    }

    // Module-level subjects.
    for (index, entry) in snapshot.symbols.symbols.iter().enumerate() {
        if matches!(
            entry.kind,
            SymbolKind::Function | SymbolKind::FiniteType | SymbolKind::RecordType
        ) {
            collector.consider(candidate_from(uri, snapshot, index));
        }
    }

    // Builtins and keywords.
    for builtin in indexes::BUILTIN_TYPE_NAMES {
        collector.consider(CompletionCandidate {
            label: builtin.to_owned(),
            class: CompletionClass::BuiltinType,
            detail: Some("builtin scalar type".to_owned()),
        });
    }
    if filter_by_prefix {
        for keyword in KEYWORDS {
            collector.consider(CompletionCandidate {
                label: keyword.to_owned(),
                class: CompletionClass::Keyword,
                detail: None,
            });
        }
    }

    collector.finish(true)
}

const MAX_COMPLETION_ITEMS: usize = 200;

fn function_at(ast: &AbstractSyntaxTree, byte: usize) -> Option<&AstFunction> {
    ast.functions
        .iter()
        .find(|function| function.span.start <= byte && byte <= function.span.end)
}

/// Collect `(name, type)` pairs for local bindings introduced strictly before
/// `byte` along the sequential statement path that contains it, mirroring
/// elaboration order closely enough to stay high-confidence.
fn collect_visible_locals(
    statements: &[AstStmt],
    byte: usize,
    visible: &mut Vec<(String, String)>,
) {
    for statement in statements {
        match statement {
            AstStmt::Let {
                name, value_type, ..
            } => {
                if name.span.end < byte {
                    visible.push((name.text.clone(), value_type.text.clone()));
                } else if name.span.contains_byte(byte) {
                    return;
                }
            }
            AstStmt::BoundedIteration {
                name, state_type, ..
            } => {
                if name.span.end < byte {
                    visible.push((name.text.clone(), state_type.text.clone()));
                }
            }
            AstStmt::If {
                then_body,
                else_body,
                span,
                ..
            } => {
                if span.contains_byte(byte) {
                    if statements_contain(then_body, byte) {
                        collect_visible_locals(then_body, byte, visible);
                    } else if statements_contain(else_body, byte) {
                        collect_visible_locals(else_body, byte, visible);
                    }
                    return;
                }
                // Statements fully before the cursor contribute nothing here;
                // bindings inside completed branches are not visible after.
            }
            AstStmt::Fail { .. } | AstStmt::Return { .. } => {}
        }
    }
}

trait SpanExt {
    fn contains_byte(&self, byte: usize) -> bool;
}

impl SpanExt for mncs_syntax::SourceSpan {
    fn contains_byte(&self, byte: usize) -> bool {
        self.start <= byte && byte <= self.end
    }
}

fn statements_contain(statements: &[AstStmt], byte: usize) -> bool {
    statements
        .iter()
        .any(|statement| stmt_span(statement).contains_byte(byte))
}

fn stmt_span(statement: &AstStmt) -> mncs_syntax::SourceSpan {
    match statement {
        AstStmt::Let { span, .. }
        | AstStmt::If { span, .. }
        | AstStmt::Fail { span, .. }
        | AstStmt::Return { span, .. }
        | AstStmt::BoundedIteration { span, .. } => *span,
    }
}

#[cfg(test)]
mod completion_prefix_tests {
    use super::*;

    #[test]
    fn completion_prefix_collection_executes_bounded_native_batches() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let library = [
            manifest_dir.join("../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../../mncs-stdlib/library"),
        ]
        .into_iter()
        .find(|path| path.join("core/sequences.mncs").is_file())
        .expect("sibling MNCS standard library");
        std::env::set_var("MNCS_LIBRARY_PATH", &library);

        let cache = std::sync::RwLock::new(None);
        let store = crate::document::DocumentStore::new(None);
        let mut native_calls = 0;
        let mut native_prefix_matches = |labels: &[String], prefix: &str| {
            native_calls += 1;
            assert!(labels.len() <= crate::native_filter::MAX_TEXT_MATCH_ITEMS);
            let names = labels
                .iter()
                .map(|label| label.as_bytes().to_vec())
                .collect::<Vec<_>>();
            let native = crate::native_filter::execute_prefix_matches(
                &cache,
                &store,
                &names,
                prefix.as_bytes(),
            )
            .expect("MNCS native completion prefix execution");
            if !native.valid || native.input_count != labels.len() {
                panic!("native completion prefix result failed validation");
            }
            Some(
                native
                    .matched
                    .get(..labels.len())
                    .expect("native prefix mask covers the input window")
                    .to_vec(),
            )
        };
        let mut collector =
            CompletionCollector::new("alpha_", true, true, &mut native_prefix_matches);
        for index in 0..130 {
            let (label, class) = if index % 2 == 0 {
                (format!("alpha_{index}"), CompletionClass::Keyword)
            } else {
                (format!("beta_{index}"), CompletionClass::Keyword)
            };
            collector.consider(CompletionCandidate {
                label,
                class,
                detail: None,
            });
        }
        collector.consider(CompletionCandidate {
            label: "alpha_0".to_owned(),
            class: CompletionClass::Keyword,
            detail: None,
        });
        let candidates = collector.finish(true);

        let mut expected: Vec<String> = (0..130)
            .filter(|index| index % 2 == 0)
            .map(|index| format!("alpha_{index}"))
            .collect();
        expected.sort();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(native_calls, 3, "131 candidates use 64-item native windows");
    }
}
