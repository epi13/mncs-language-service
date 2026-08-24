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

pub(crate) const KEYWORDS: [&str; 24] = [
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
    "effect",
    "capability",
    "authorized_by",
    "enum",
    "record",
    "match",
    "iterate",
    "up_to",
    "carrying",
    "next",
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
                            .map(|variant| variant.text.as_str())
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
            TokenKind::IntegerLiteral => Some(TokenClass::Number),
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
            | TokenKind::EffectKeyword
            | TokenKind::CapabilityKeyword
            | TokenKind::AuthorizedKeyword
            | TokenKind::EnumKeyword
            | TokenKind::RecordKeyword
            | TokenKind::MatchKeyword
            | TokenKind::IterateKeyword
            | TokenKind::UpToKeyword
            | TokenKind::CarryingKeyword
            | TokenKind::NextKeyword
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

    if member_context {
        return member_completions(uri, snapshot, cursor_index, &significant);
    }

    scoped_candidates(uri, snapshot, byte, &prefix, !prefix.is_empty())
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
        let entry = &snapshot.symbols.symbols[reference.target];
        return match entry.kind {
            SymbolKind::FiniteType => variants_of(uri, snapshot, reference.target),
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
) -> Vec<CompletionCandidate> {
    let mut seen = BTreeSet::new();
    let mut items: Vec<CompletionCandidate> = Vec::new();

    fn consider(
        items: &mut Vec<CompletionCandidate>,
        seen: &mut BTreeSet<String>,
        candidate: CompletionCandidate,
        prefix: &str,
        filter_by_prefix: bool,
    ) {
        if filter_by_prefix && !candidate.label.starts_with(prefix) {
            return;
        }
        if seen.insert(candidate.label.clone()) {
            items.push(candidate);
        }
    }

    // Visible locals along the elaboration path to the cursor.
    if let Some(ast) = snapshot.front_end.ast.as_ref() {
        if let Some(function) = function_at(ast, byte) {
            for parameter in &function.inputs {
                consider(
                    &mut items,
                    &mut seen,
                    CompletionCandidate {
                        label: parameter.name.text.clone(),
                        class: CompletionClass::Symbol(SymbolKind::Parameter),
                        detail: Some(format!(
                            "{}: {}",
                            parameter.name.text, parameter.value_type.text
                        )),
                    },
                    prefix,
                    filter_by_prefix,
                );
            }
            let mut visible = Vec::new();
            collect_visible_locals(&function.body.statements, byte, &mut visible);
            for (name, _type_name) in visible {
                consider(
                    &mut items,
                    &mut seen,
                    CompletionCandidate {
                        label: name,
                        class: CompletionClass::Variable,
                        detail: None,
                    },
                    prefix,
                    filter_by_prefix,
                );
            }
        }
    }

    // Module-level subjects.
    for (index, entry) in snapshot.symbols.symbols.iter().enumerate() {
        if matches!(
            entry.kind,
            SymbolKind::Function | SymbolKind::FiniteType | SymbolKind::RecordType
        ) {
            consider(
                &mut items,
                &mut seen,
                candidate_from(uri, snapshot, index),
                prefix,
                filter_by_prefix,
            );
        }
    }

    // Builtins and keywords.
    for builtin in indexes::BUILTIN_TYPE_NAMES {
        consider(
            &mut items,
            &mut seen,
            CompletionCandidate {
                label: builtin.to_owned(),
                class: CompletionClass::BuiltinType,
                detail: Some("builtin scalar type".to_owned()),
            },
            prefix,
            filter_by_prefix,
        );
    }
    if filter_by_prefix {
        for keyword in KEYWORDS {
            consider(
                &mut items,
                &mut seen,
                CompletionCandidate {
                    label: keyword.to_owned(),
                    class: CompletionClass::Keyword,
                    detail: None,
                },
                prefix,
                filter_by_prefix,
            );
        }
    }

    items.sort_by(|left, right| left.label.cmp(&right.label));
    items.truncate(MAX_COMPLETION_ITEMS);
    items
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
