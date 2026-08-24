//! Symbol and reference indexes derived from authoritative frontend artifacts.
//!
//! Declarations come from the lossless AST (which carries exact spans);
//! semantic identities, signatures, contracts, effects, capabilities, and
//! obligations come from the elaborated `mncs-model` program; use sites come
//! from the compiler's authoritative [`NameResolutionIndex`]. Nothing here
//! re-implements binding or typing rules: the index only joins and projects
//! what `mncs-language` already decided.

use std::collections::BTreeMap;

use mncs_compiler::{ResolvedNameKind, SourceFrontEndResult};
use mncs_model::{ContractKind, EvidenceStatus, ObligationStatus, SemanticId};
use mncs_syntax::{AstFunction, AstStmt};

/// Service-level classification of an indexed symbol.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Function,
    Parameter,
    Binding,
    IterationState,
    FiniteType,
    FiniteVariant,
    RecordType,
    RecordField,
}

impl SymbolKind {
    fn from_resolved(kind: ResolvedNameKind) -> Self {
        match kind {
            ResolvedNameKind::Function => Self::Function,
            ResolvedNameKind::Parameter => Self::Parameter,
            ResolvedNameKind::Binding => Self::Binding,
            ResolvedNameKind::IterationState => Self::IterationState,
            ResolvedNameKind::FiniteType => Self::FiniteType,
            ResolvedNameKind::FiniteVariant => Self::FiniteVariant,
            ResolvedNameKind::RecordType => Self::RecordType,
            ResolvedNameKind::RecordField => Self::RecordField,
        }
    }
}

/// One indexed declaration. Spans are byte spans from the AST; projection into
/// protocol coordinates happens at query time via the snapshot's position map.
#[derive(Debug, Clone)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    /// Byte span of the name token itself.
    pub name_span: mncs_syntax::SourceSpan,
    /// Byte span of the whole declaration.
    pub full_span: mncs_syntax::SourceSpan,
    /// Enclosing function's simple name when the symbol is local to one.
    pub container: Option<String>,
    /// Authoritative module-qualified semantic identity where the language
    /// defines one (functions, finite types/variants, record types).
    pub identity: Option<SemanticId>,
    /// Declared type text for value-like symbols.
    pub type_name: Option<String>,
    /// Index of the parent symbol for variants and fields.
    pub parent: Option<usize>,
}

impl SymbolEntry {
    /// Human-readable signature/detail line shared by hover and MCP describe.
    pub fn detail(&self) -> Option<String> {
        match self.kind {
            SymbolKind::Module | SymbolKind::FiniteVariant | SymbolKind::RecordField => None,
            _ => self.type_name.clone().map(|ty| format!(": {ty}")),
        }
    }
}

/// One indexed reference occurrence pointing at a declaration entry.
#[derive(Debug, Clone)]
pub struct ReferenceEntry {
    pub occurrence_span: mncs_syntax::SourceSpan,
    pub kind: SymbolKind,
    /// Index into `SymbolIndex::symbols`.
    pub target: usize,
}

/// Per-document symbol/reference index.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    pub symbols: Vec<SymbolEntry>,
    /// Sorted by occurrence start byte for binary search.
    pub references: Vec<ReferenceEntry>,
    /// Declaration index by exact name-span start byte.
    declarations_by_start: BTreeMap<usize, usize>,
    /// Reference groups keyed by target symbol index.
    references_by_target: BTreeMap<usize, Vec<ReferenceEntry>>,
    /// Function entries' body statement roots retained for scoped completion.
    functions: BTreeMap<String, usize>,
}

impl SymbolIndex {
    pub(crate) fn build(front_end: &SourceFrontEndResult) -> Self {
        let mut index = Self::default();
        let Some(ast) = front_end.ast.as_ref() else {
            return index;
        };

        let program = front_end.program.as_ref();
        let module_symbol = index.push(SymbolEntry {
            name: ast.module.text.clone(),
            kind: SymbolKind::Module,
            name_span: ast.module.span,
            full_span: ast.module.span,
            container: None,
            identity: None,
            type_name: None,
            parent: None,
        });
        index
            .declarations_by_start
            .insert(ast.module.span.start, module_symbol);

        for declared in &ast.finite_types {
            let parent = index.push(SymbolEntry {
                name: declared.name.text.clone(),
                kind: SymbolKind::FiniteType,
                name_span: declared.name.span,
                full_span: declared.span,
                container: None,
                identity: program.and_then(|program| {
                    program
                        .finite_types
                        .iter()
                        .find(|candidate| candidate.name == declared.name.text)
                        .map(|candidate| candidate.identity.clone())
                }),
                type_name: None,
                parent: None,
            });
            index
                .declarations_by_start
                .insert(declared.name.span.start, parent);
            for variant in &declared.variants {
                let entry = index.push(SymbolEntry {
                    name: variant.text.clone(),
                    kind: SymbolKind::FiniteVariant,
                    name_span: variant.span,
                    full_span: variant.span,
                    container: Some(declared.name.text.clone()),
                    identity: program.and_then(|program| {
                        program
                            .finite_types
                            .iter()
                            .find(|candidate| candidate.name == declared.name.text)
                            .and_then(|candidate| {
                                candidate
                                    .variants
                                    .iter()
                                    .find(|variant_candidate| {
                                        variant_candidate.name == variant.text
                                    })
                                    .map(|found| found.identity.clone())
                            })
                    }),
                    type_name: None,
                    parent: Some(parent),
                });
                index
                    .declarations_by_start
                    .insert(variant.span.start, entry);
            }
        }

        for declared in &ast.record_types {
            let parent = index.push(SymbolEntry {
                name: declared.name.text.clone(),
                kind: SymbolKind::RecordType,
                name_span: declared.name.span,
                full_span: declared.span,
                container: None,
                identity: program.and_then(|program| {
                    program
                        .record_types
                        .iter()
                        .find(|candidate| candidate.name == declared.name.text)
                        .map(|candidate| candidate.identity.clone())
                }),
                type_name: None,
                parent: None,
            });
            index
                .declarations_by_start
                .insert(declared.name.span.start, parent);
            for field in &declared.fields {
                let entry = index.push(SymbolEntry {
                    name: field.name.text.clone(),
                    kind: SymbolKind::RecordField,
                    name_span: field.name.span,
                    full_span: field.span,
                    container: Some(declared.name.text.clone()),
                    identity: None,
                    type_name: Some(field.value_type.text.clone()),
                    parent: Some(parent),
                });
                index
                    .declarations_by_start
                    .insert(field.name.span.start, entry);
            }
        }

        for function in &ast.functions {
            let entry = index.push(SymbolEntry {
                name: function.name.text.clone(),
                kind: SymbolKind::Function,
                name_span: function.name.span,
                full_span: function.span,
                container: None,
                identity: program
                    .map(|program| mncs_model::function_id(&program.module, &function.name.text)),
                type_name: function
                    .outputs
                    .first()
                    .map(|output| output.value_type.text.clone()),
                parent: None,
            });
            index
                .declarations_by_start
                .insert(function.name.span.start, entry);
            index.functions.insert(function.name.text.clone(), entry);

            for parameter in &function.inputs {
                let param = index.push(SymbolEntry {
                    name: parameter.name.text.clone(),
                    kind: SymbolKind::Parameter,
                    name_span: parameter.name.span,
                    full_span: parameter.span,
                    container: Some(function.name.text.clone()),
                    identity: None,
                    type_name: Some(parameter.value_type.text.clone()),
                    parent: Some(entry),
                });
                index
                    .declarations_by_start
                    .insert(parameter.name.span.start, param);
            }

            index.collect_binding_declarations(function, entry);
        }
        // Reference occurrences from the authoritative resolution index.
        for resolution in &front_end.name_resolutions.resolutions {
            let Some(&target) = index
                .declarations_by_start
                .get(&resolution.declaration.start)
            else {
                continue;
            };
            let kind = SymbolKind::from_resolved(resolution.kind);
            index.references.push(ReferenceEntry {
                occurrence_span: resolution.occurrence,
                kind,
                target,
            });
        }
        index
            .references
            .sort_by_key(|entry| entry.occurrence_span.start);
        for entry in &index.references {
            index
                .references_by_target
                .entry(entry.target)
                .or_default()
                .push(entry.clone());
        }

        index
    }

    fn push(&mut self, entry: SymbolEntry) -> usize {
        self.symbols.push(entry);
        self.symbols.len() - 1
    }

    fn collect_binding_declarations(&mut self, function: &AstFunction, owner: usize) {
        // Walk statements in elaboration order recording let bindings and
        // iteration identities with their exact name spans.
        fn walk(index: &mut SymbolIndex, statements: &[AstStmt], function: &str, owner: usize) {
            for statement in statements {
                match statement {
                    AstStmt::Let {
                        name, value_type, ..
                    } => {
                        let entry = index.push(SymbolEntry {
                            name: name.text.clone(),
                            kind: SymbolKind::Binding,
                            name_span: name.span,
                            full_span: name.span,
                            container: Some(function.to_owned()),
                            identity: None,
                            type_name: Some(value_type.text.clone()),
                            parent: Some(owner),
                        });
                        index.declarations_by_start.insert(name.span.start, entry);
                    }
                    AstStmt::BoundedIteration {
                        name, state_type, ..
                    } => {
                        let entry = index.push(SymbolEntry {
                            name: name.text.clone(),
                            kind: SymbolKind::IterationState,
                            name_span: name.span,
                            full_span: name.span,
                            container: Some(function.to_owned()),
                            identity: None,
                            type_name: Some(state_type.text.clone()),
                            parent: Some(owner),
                        });
                        index.declarations_by_start.insert(name.span.start, entry);
                    }
                    AstStmt::If {
                        then_body,
                        else_body,
                        ..
                    } => {
                        walk(index, then_body, function, owner);
                        walk(index, else_body, function, owner);
                    }
                    AstStmt::Fail { .. } | AstStmt::Return { .. } => {}
                }
            }
        }
        walk(self, &function.body.statements, &function.name.text, owner);
    }

    /// The symbol whose *name token* contains `byte`, if any. Ties are broken
    /// toward the tightest containing span.
    pub fn declaration_at(&self, byte: usize) -> Option<usize> {
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.name_span.start <= byte && byte <= entry.name_span.end)
            .min_by_key(|(_, entry)| {
                let span = entry.name_span;
                (byte - span.start) + (span.end - byte)
            })
            .map(|(position, _)| position)
    }

    /// All reference occurrences whose span contains `byte`.
    pub fn references_at(&self, byte: usize) -> impl Iterator<Item = &ReferenceEntry> {
        self.references.iter().filter(move |entry| {
            entry.occurrence_span.start <= byte && byte <= entry.occurrence_span.end
        })
    }

    /// References grouped by target declaration.
    pub fn references_to(&self, target: usize) -> &[ReferenceEntry] {
        self.references_by_target
            .get(&target)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn function_entry(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }
}

/// Rendered scalar type names recognized by the current source profiles.
pub const BUILTIN_TYPE_NAMES: [&str; 9] =
    ["bool", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"];

pub(crate) fn is_builtin_type(name: &str) -> bool {
    BUILTIN_TYPE_NAMES.contains(&name)
}

/// Contract kind label preserving the language's own vocabulary.
pub(crate) fn contract_kind_label(kind: &ContractKind) -> &'static str {
    match kind {
        ContractKind::Requires => "requires",
        ContractKind::Ensures => "ensures",
        ContractKind::Invariant => "invariant",
        ContractKind::Preserves => "preserves",
        ContractKind::Budget => "budget",
    }
}

pub(crate) fn evidence_status_label(status: &EvidenceStatus) -> &'static str {
    match status {
        EvidenceStatus::Claimed => "claimed",
        EvidenceStatus::Tested => "tested",
        EvidenceStatus::Analyzed => "analyzed",
        EvidenceStatus::Verified => "verified",
        EvidenceStatus::ExternallyVerified => "externally_verified",
    }
}

pub(crate) fn obligation_status_label(status: &ObligationStatus) -> &'static str {
    match status {
        ObligationStatus::Pass => "pass",
        ObligationStatus::Fail => "fail",
        ObligationStatus::Unknown => "unknown",
    }
}
