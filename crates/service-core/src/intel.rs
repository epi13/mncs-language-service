//! Higher-level semantic intelligence: signature help, declaration and type
//! definition, syntax-aware selection ranges, call hierarchy, and inlay hints.
//!
//! Every query here joins authoritative `mncs-language` artifacts (AST, name
//! resolutions, elaborated program) exactly like the base queries in
//! [`crate::queries`]; nothing re-implements binding, typing, or resolution.
//! Two deliberate design points:
//!
//! * Signature help is token-driven rather than AST-driven so it keeps working
//!   mid-typing, when the document no longer parses and an AST may not exist.
//! * MNCS has no separate declaration/definition split (one declaration site
//!   per symbol, no headers or forward declarations), so `declaration`
//!   resolves to the same site as `definition`. The separate entry point
//!   exists so clients can advertise honestly; it is not a second semantic.

use std::sync::Arc;

use mncs_syntax::TokenKind;
use serde::{Deserialize, Serialize};

use crate::analysis::DocumentAnalysis;
use crate::coords::RangeInfo;
use crate::indexes::SymbolKind;
use crate::queries::{
    snapshot_info, summarize, Range, ResponseStatus, ServiceError, SymbolSummary,
};

// ---------------------------------------------------------------------------
// Signature help
// ---------------------------------------------------------------------------

/// One parameter of a callable signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureParameter {
    pub name: String,
    pub type_name: String,
}

/// A callable signature with the cursor's active argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureHelpResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::queries::SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee: Option<Box<SymbolSummary>>,
    /// Canonical one-line signature, mirroring hover rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<SignatureParameter>,
    pub active_parameter: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_parameters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
}

use crate::queries::LanguageService;

impl LanguageService {
    /// Signature help at `line:character`.
    ///
    /// Finds the innermost unclosed-or-containing `(...)` group around the
    /// cursor whose opening paren directly follows an identifier, resolves
    /// that identifier to a function declaration (same document first, then
    /// the workspace), and reports the active argument by counting top-level
    /// commas before the cursor.
    pub fn signature_help(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<SignatureHelpResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);

        let Some((callee_name, active_parameter)) = enclosing_call(&snapshot, byte) else {
            return Ok(SignatureHelpResponse {
                status: ResponseStatus::Unresolved {
                    reason: "the cursor is not inside a call argument list".to_owned(),
                },
                snapshot: Some(info()),
                callee: None,
                label: None,
                parameters: Vec::new(),
                active_parameter: 0,
                generic_parameters: Vec::new(),
                return_type: None,
            });
        };

        let Some((owner_uri, owner_snapshot, index)) =
            self.resolve_function(&snapshot, uri, &callee_name)
        else {
            return Ok(SignatureHelpResponse {
                status: ResponseStatus::Unresolved {
                    reason: format!(
                        "call target `{callee_name}` does not resolve to a known function"
                    ),
                },
                snapshot: Some(info()),
                callee: None,
                label: None,
                parameters: Vec::new(),
                active_parameter: 0,
                generic_parameters: Vec::new(),
                return_type: None,
            });
        };

        let signature = function_signature(&owner_snapshot, index);
        let active_parameter = if signature.parameters.is_empty() {
            0
        } else {
            active_parameter.min(signature.parameters.len() - 1)
        };
        Ok(SignatureHelpResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            callee: Some(Box::new(summarize(&owner_uri, &owner_snapshot, index))),
            label: Some(signature.label),
            parameters: signature.parameters,
            active_parameter,
            generic_parameters: signature.generic_parameters,
            return_type: signature.return_type,
        })
    }

    /// Resolve a function name to its declaration, preferring the querying
    /// document and otherwise taking the first workspace match by URI.
    /// Returns the owning URI, its snapshot, and the symbol index.
    pub(crate) fn resolve_function(
        &self,
        snapshot: &DocumentAnalysis,
        uri: &str,
        name: &str,
    ) -> Option<(String, Arc<DocumentAnalysis>, usize)> {
        if let Some(index) = snapshot
            .symbols
            .symbols
            .iter()
            .position(|entry| entry.kind == SymbolKind::Function && entry.name == name)
        {
            // `snapshot` is borrowed; re-fetch the cached Arc for the return.
            let owned = self.cached_snapshot_arc(uri)?;
            return Some((uri.to_owned(), owned, index));
        }
        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            if candidate_uri == uri {
                continue;
            }
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                continue;
            };
            if let Some(index) = candidate
                .symbols
                .symbols
                .iter()
                .position(|entry| entry.kind == SymbolKind::Function && entry.name == name)
            {
                return Some((candidate_uri, candidate, index));
            }
        }
        None
    }

    fn cached_snapshot_arc(&self, uri: &str) -> Option<Arc<DocumentAnalysis>> {
        self.snapshot(uri).ok()
    }
}

pub(crate) struct FunctionSignature {
    pub label: String,
    pub parameters: Vec<SignatureParameter>,
    pub generic_parameters: Vec<String>,
    pub return_type: Option<String>,
}

pub(crate) fn function_signature(snapshot: &DocumentAnalysis, index: usize) -> FunctionSignature {
    let entry = &snapshot.symbols.symbols[index];
    let found = snapshot.front_end.ast.as_ref().and_then(|ast| {
        ast.functions
            .iter()
            .find(|function| function.name.text == entry.name)
    });
    let inputs = found
        .map(|function| function.inputs.clone())
        .unwrap_or_default();
    let outputs = found
        .map(|function| function.outputs.clone())
        .unwrap_or_default();
    let generics = found
        .map(|function| function.generic_params.clone())
        .unwrap_or_default();
    let parameters = inputs
        .iter()
        .map(|parameter| SignatureParameter {
            name: parameter.name.text.clone(),
            type_name: parameter.value_type.text.clone(),
        })
        .collect::<Vec<_>>();
    let generic_parameters: Vec<String> = generics
        .iter()
        .map(|param| param.name.text.clone())
        .collect();
    let return_type = outputs.first().map(|output| output.value_type.text.clone());
    let label = render_signature_label(&entry.name, &generic_parameters, &inputs, &outputs);
    FunctionSignature {
        label,
        parameters,
        generic_parameters,
        return_type,
    }
}

fn render_signature_label(
    name: &str,
    generics: &[String],
    inputs: &[mncs_syntax::AstParameter],
    outputs: &[mncs_syntax::AstParameter],
) -> String {
    let generics = if generics.is_empty() {
        String::new()
    } else {
        format!("<{}>", generics.join(", "))
    };
    let inputs = inputs
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name.text, parameter.value_type.text))
        .collect::<Vec<_>>()
        .join(", ");
    let outputs = outputs
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name.text, parameter.value_type.text))
        .collect::<Vec<_>>()
        .join(", ");
    format!("fn {name}{generics}({inputs}) -> ({outputs})")
}

/// Locate the innermost call group around `byte`: returns the callee
/// identifier text and the count of top-level commas before the cursor.
fn enclosing_call(snapshot: &DocumentAnalysis, byte: usize) -> Option<(String, usize)> {
    let tokens: Vec<&mncs_syntax::SourceToken> = snapshot
        .front_end
        .lexical
        .tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    // Match parens; each LeftParen maps to its RightParen index (or None for
    // groups still open, the normal mid-typing state).
    let mut mates: Vec<Option<usize>> = vec![None; tokens.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::LeftParen => stack.push(index),
            TokenKind::RightParen => {
                if let Some(open) = stack.pop() {
                    mates[open] = Some(index);
                }
            }
            _ => {}
        }
    }
    // Innermost group covering the cursor.
    let mut best: Option<usize> = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::LeftParen || token.span.start > byte {
            continue;
        }
        let inside = match mates[index] {
            Some(close) => byte <= tokens[close].span.end,
            None => true,
        };
        if inside && best.is_none_or(|prev| tokens[prev].span.start < token.span.start) {
            best = Some(index);
        }
    }
    let open = best?;
    // The callee is the significant token directly before `(`.
    let callee = tokens[..open].last()?;
    if callee.kind != TokenKind::Identifier || callee.span.end > tokens[open].span.start {
        return None;
    }
    // Count top-level commas strictly before the cursor, stopping at the
    // group's own close paren.
    let close = mates[open].unwrap_or(tokens.len());
    let mut active = 0usize;
    let mut depth = 0usize;
    for token in &tokens[open + 1..close.min(tokens.len())] {
        if token.span.end > byte {
            break;
        }
        match token.kind {
            TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::LeftBrace => depth += 1,
            TokenKind::RightParen | TokenKind::RightBracket | TokenKind::RightBrace => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => active += 1,
            _ => {}
        }
    }
    Some((callee.text.clone(), active))
}

// ---------------------------------------------------------------------------
// Declaration / type definition
// ---------------------------------------------------------------------------

impl LanguageService {
    /// Go to declaration. MNCS has a single declaration site per symbol (no
    /// headers, interfaces, or forward declarations), so this resolves to the
    /// same site as go-to-definition; the entry point stays distinct so the
    /// protocol shape remains honest if the language ever grows a split.
    pub fn declaration(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<crate::queries::DefinitionResponse, ServiceError> {
        self.definition(uri, line, character)
    }

    /// Go to type definition: resolve the symbol under the cursor to its
    /// declared type, then to that type's declaration (workspace-wide).
    /// Builtin scalar types have no declaration site and are reported as
    /// unresolved rather than guessed.
    pub fn type_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<crate::queries::DefinitionResponse, ServiceError> {
        use crate::queries::DefinitionResponse;

        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);

        let type_name = if let Some(index) = self.primary_symbol_index(&snapshot, byte) {
            symbol_type_name(&snapshot, index)
        } else if let Some(reference) = snapshot.symbols.references_at(byte).next() {
            reference.target.and_then(|target| {
                let summary = summarize(uri, &snapshot, target);
                let target_uri = summary.uri.clone()?;
                let target_snapshot = self.snapshot(&target_uri).ok()?;
                let target_index = target_snapshot.symbols.symbols.iter().position(|entry| {
                    entry.name == summary.name
                        && entry.name_span.start == summary.name_range.start_byte
                        && entry.kind == summary.kind
                })?;
                symbol_type_name(&target_snapshot, target_index)
            })
        } else {
            None
        };

        let Some(type_name) = type_name else {
            return Ok(DefinitionResponse {
                status: ResponseStatus::Unresolved {
                    reason: "no typed symbol resolves at this position".to_owned(),
                },
                snapshot: Some(info()),
                definitions: Vec::new(),
            });
        };
        if crate::indexes::is_builtin_type(&type_name) {
            return Ok(DefinitionResponse {
                status: ResponseStatus::Unresolved {
                    reason: format!("builtin scalar type `{type_name}` has no declaration site"),
                },
                snapshot: Some(info()),
                definitions: Vec::new(),
            });
        }
        let mut definitions = Vec::new();
        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                continue;
            };
            for index in 0..candidate.symbols.symbols.len() {
                let entry = &candidate.symbols.symbols[index];
                if entry.name == type_name
                    && matches!(entry.kind, SymbolKind::FiniteType | SymbolKind::RecordType)
                {
                    definitions.push(summarize(&candidate_uri, &candidate, index));
                }
            }
        }
        definitions.sort_by(|left, right| left.uri.cmp(&right.uri));
        definitions.dedup();
        let status = if definitions.is_empty() {
            ResponseStatus::Unresolved {
                reason: format!("no declaration of type `{type_name}` is visible"),
            }
        } else {
            ResponseStatus::Answered
        };
        Ok(DefinitionResponse {
            status,
            snapshot: Some(info()),
            definitions,
        })
    }
}

/// Declared type name for a value-like symbol, if the language records one.
fn symbol_type_name(snapshot: &DocumentAnalysis, index: usize) -> Option<String> {
    let entry = &snapshot.symbols.symbols[index];
    match entry.kind {
        SymbolKind::Parameter | SymbolKind::Binding | SymbolKind::IterationState => {
            entry.type_name.clone()
        }
        SymbolKind::RecordField => entry.type_name.clone(),
        SymbolKind::Function => snapshot
            .front_end
            .ast
            .as_ref()?
            .functions
            .iter()
            .find(|function| function.name.text == entry.name)?
            .outputs
            .first()
            .map(|output| output.value_type.text.clone()),
        SymbolKind::FiniteType | SymbolKind::RecordType => Some(entry.name.clone()),
        SymbolKind::FiniteVariant => entry.container.as_ref().and_then(|parent| {
            snapshot
                .front_end
                .ast
                .as_ref()
                .and_then(|ast| {
                    ast.finite_types
                        .iter()
                        .find(|declared| &declared.name.text == parent)
                })
                .map(|_| parent.clone())
        }),
        SymbolKind::Module => None,
    }
}

// ---------------------------------------------------------------------------
// Selection ranges
// ---------------------------------------------------------------------------

/// One cursor's selection chain, innermost range first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionChain {
    /// Innermost range first, outermost (whole document) last.
    pub ranges: Vec<Range>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionRangesResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::queries::SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chains: Vec<SelectionChain>,
}

impl LanguageService {
    /// Syntax-aware selection chains from the CST: each requested position
    /// yields its innermost-to-outermost containing node spans.
    pub fn selection_ranges(
        &self,
        uri: &str,
        positions: &[(u32, u32)],
    ) -> Result<SelectionRangesResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let mut chains = Vec::new();
        for (line, character) in positions {
            let byte = snapshot.positions.offset_of(text, *line, *character);
            let mut chain: Vec<&mncs_syntax::CstNode> = Vec::new();
            collect_cst_chain(&snapshot.front_end.cst.root, byte, &mut chain);
            let mut ranges = chain
                .iter()
                .rev()
                .map(|node| snapshot.positions.range_of(text, node.span))
                .collect::<Vec<_>>();
            ranges.dedup();
            chains.push(SelectionChain { ranges });
        }
        Ok(SelectionRangesResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            chains,
        })
    }
}

fn collect_cst_chain<'a>(
    node: &'a mncs_syntax::CstNode,
    byte: usize,
    chain: &mut Vec<&'a mncs_syntax::CstNode>,
) {
    if node.span.start > byte || byte > node.span.end {
        return;
    }
    chain.push(node);
    // Tightest containing child wins; siblings are disjoint by construction.
    let mut best: Option<&'a mncs_syntax::CstNode> = None;
    for child in &node.children {
        if child.span.start <= byte && byte <= child.span.end {
            let tight = |node: &mncs_syntax::CstNode| node.span.end - node.span.start;
            if best.is_none_or(|prev| tight(child) < tight(prev)) {
                best = Some(child);
            }
        }
    }
    if let Some(child) = best {
        collect_cst_chain(child, byte, chain);
    }
}

// ---------------------------------------------------------------------------
// Call hierarchy
// ---------------------------------------------------------------------------

/// One item in the MNCS call hierarchy (always a function: MNCS has no
/// methods, lambdas, or function pointers, so every call edge lands on a
/// module-level function identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: SymbolKind,
    /// Module-qualified semantic identity; the stable key for
    /// incoming/outgoing queries.
    pub identity: Option<String>,
    pub uri: String,
    pub range: Range,
    pub name_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One hierarchy edge with the call-site ranges in the neighboring document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyEdge {
    pub item: CallHierarchyItem,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_ranges: Vec<Range>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareCallHierarchyResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::queries::SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<CallHierarchyItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyCallsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::queries::SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<CallHierarchyEdge>,
}

impl LanguageService {
    /// Hierarchy preparation: the enclosing function at the cursor, if any.
    pub fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PrepareCallHierarchyResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);
        let index = self
            .primary_symbol_index(&snapshot, byte)
            .and_then(|index| {
                // References resolve to their target; declarations stand alone.
                let entry = &snapshot.symbols.symbols[index];
                if entry.kind == SymbolKind::Function {
                    Some(index)
                } else {
                    enclosing_function(&snapshot, byte)
                }
            });
        let Some(index) = index else {
            return Ok(PrepareCallHierarchyResponse {
                status: ResponseStatus::Unresolved {
                    reason: "the cursor is not on or inside a function".to_owned(),
                },
                snapshot: Some(info()),
                items: Vec::new(),
            });
        };
        Ok(PrepareCallHierarchyResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            items: vec![hierarchy_item(uri, &snapshot, index)],
        })
    }

    /// Callers of `identity`, workspace-wide, grouped by calling function.
    /// Call sites come from the compiler's authoritative name resolutions,
    /// never from text search.
    pub fn incoming_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let Some((callee_uri, callee_snapshot, callee_index)) = self.symbol_by_identity(identity)
        else {
            return Ok(CallHierarchyCallsResponse {
                status: ResponseStatus::Unresolved {
                    reason: format!("identity {identity} names no workspace symbol"),
                },
                snapshot: Some(info()),
                subject_identity: Some(identity.to_owned()),
                edges: Vec::new(),
            });
        };
        let callee = &callee_snapshot.symbols.symbols[callee_index];
        if callee.kind != SymbolKind::Function {
            return Ok(CallHierarchyCallsResponse {
                status: ResponseStatus::Unresolved {
                    reason: "only functions participate in the call hierarchy".to_owned(),
                },
                snapshot: Some(info()),
                subject_identity: Some(identity.to_owned()),
                edges: Vec::new(),
            });
        }
        let declaration = callee.name_span;
        let _ = callee_uri;
        // Group call sites by (document, enclosing function).
        let mut groups: std::collections::BTreeMap<(String, usize), Vec<RangeInfo>> =
            std::collections::BTreeMap::new();
        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                continue;
            };
            let text = candidate.text();
            for reference in &candidate.symbols.references {
                if reference.kind != SymbolKind::Function
                    || reference.declaration_span != declaration
                {
                    continue;
                }
                let Some(caller) = enclosing_function(&candidate, reference.occurrence_span.start)
                else {
                    continue;
                };
                groups
                    .entry((candidate_uri.clone(), caller))
                    .or_default()
                    .push(
                        candidate
                            .positions
                            .range_of(text, reference.occurrence_span),
                    );
            }
        }
        let mut edges = Vec::new();
        for ((caller_uri, caller_index), from_ranges) in groups {
            let caller_snapshot = self.snapshot(&caller_uri)?;
            edges.push(CallHierarchyEdge {
                item: hierarchy_item(&caller_uri, &caller_snapshot, caller_index),
                from_ranges,
            });
        }
        edges.sort_by(|left, right| {
            (left.item.uri.clone(), left.item.name.clone())
                .cmp(&(right.item.uri.clone(), right.item.name.clone()))
        });
        Ok(CallHierarchyCallsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            subject_identity: Some(identity.to_owned()),
            edges,
        })
    }

    /// Callees of `identity` within its own document, grouped by target.
    /// Targets resolve through the same workspace join as go-to-definition,
    /// so imported callees land on their owning document's item.
    pub fn outgoing_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let Some((caller_uri, caller_snapshot, caller_index)) = self.symbol_by_identity(identity)
        else {
            return Ok(CallHierarchyCallsResponse {
                status: ResponseStatus::Unresolved {
                    reason: format!("identity {identity} names no workspace symbol"),
                },
                snapshot: Some(info()),
                subject_identity: Some(identity.to_owned()),
                edges: Vec::new(),
            });
        };
        let caller = &caller_snapshot.symbols.symbols[caller_index];
        if caller.kind != SymbolKind::Function {
            return Ok(CallHierarchyCallsResponse {
                status: ResponseStatus::Unresolved {
                    reason: "only functions participate in the call hierarchy".to_owned(),
                },
                snapshot: Some(info()),
                subject_identity: Some(identity.to_owned()),
                edges: Vec::new(),
            });
        }
        let text = caller_snapshot.text();
        let full = caller.full_span;
        // Group resolved targets by identity; each occurrence contributes its
        // own call-site range.
        let mut groups: std::collections::BTreeMap<String, (SymbolSummary, Vec<RangeInfo>)> =
            std::collections::BTreeMap::new();
        for reference in &caller_snapshot.symbols.references {
            if reference.kind != SymbolKind::Function {
                continue;
            }
            let at = reference.occurrence_span.start;
            if at < full.start || at > full.end {
                continue;
            }
            // Skip the function's own name token (a declaration, but a
            // same-spanned reference may exist for the name itself).
            if reference.occurrence_span == caller.name_span {
                continue;
            }
            for target in self.targets_for_reference(&caller_uri, &caller_snapshot, reference) {
                let key = target.identity.clone().unwrap_or_else(|| {
                    format!("{}:{}", target.uri.as_deref().unwrap_or(""), target.name)
                });
                groups
                    .entry(key)
                    .or_insert_with(|| (target.clone(), Vec::new()))
                    .1
                    .push(
                        caller_snapshot
                            .positions
                            .range_of(text, reference.occurrence_span),
                    );
            }
        }
        let mut edges = groups
            .into_values()
            .map(|(summary, mut from_ranges)| {
                from_ranges.sort_by_key(|range| range.start_byte);
                from_ranges.dedup();
                CallHierarchyEdge {
                    item: CallHierarchyItem {
                        name: summary.name,
                        kind: summary.kind,
                        identity: summary.identity,
                        uri: summary.uri.unwrap_or_else(|| caller_uri.clone()),
                        range: summary.range,
                        name_range: summary.name_range,
                        detail: summary.detail,
                    },
                    from_ranges,
                }
            })
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            (left.item.uri.clone(), left.item.name.clone())
                .cmp(&(right.item.uri.clone(), right.item.name.clone()))
        });
        Ok(CallHierarchyCallsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            subject_identity: Some(identity.to_owned()),
            edges,
        })
    }

    /// Find a workspace symbol by its module-qualified identity.
    pub(crate) fn symbol_by_identity(
        &self,
        identity: &str,
    ) -> Option<(String, Arc<DocumentAnalysis>, usize)> {
        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                continue;
            };
            if let Some(index) = candidate.symbols.symbols.iter().position(|entry| {
                entry
                    .identity
                    .as_ref()
                    .is_some_and(|id| id.as_str() == identity)
            }) {
                return Some((candidate_uri, candidate, index));
            }
        }
        None
    }
}

fn hierarchy_item(uri: &str, snapshot: &DocumentAnalysis, index: usize) -> CallHierarchyItem {
    let summary = summarize(uri, snapshot, index);
    CallHierarchyItem {
        name: summary.name,
        kind: summary.kind,
        identity: summary.identity,
        uri: summary.uri.unwrap_or_else(|| uri.to_owned()),
        range: summary.range,
        name_range: summary.name_range,
        detail: summary.detail,
    }
}

/// Innermost function whose full span contains `byte`.
fn enclosing_function(snapshot: &DocumentAnalysis, byte: usize) -> Option<usize> {
    let ast = snapshot.front_end.ast.as_ref()?;
    let function = ast
        .functions
        .iter()
        .filter(|function| function.span.start <= byte && byte <= function.span.end)
        .min_by_key(|function| function.span.end - function.span.start)?;
    snapshot
        .symbols
        .symbols
        .iter()
        .position(|entry| entry.kind == SymbolKind::Function && entry.name == function.name.text)
}

// ---------------------------------------------------------------------------
// Inlay hints
// ---------------------------------------------------------------------------

/// Hint classification. Only parameter-name hints are produced today: MNCS
/// `let` bindings carry explicit type annotations, so echoing them would be
/// noise rather than information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlayHintKind {
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlayHintItem {
    pub line: u32,
    pub character: u32,
    pub label: String,
    pub kind: InlayHintKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlayHintsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::queries::SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<InlayHintItem>,
}

impl LanguageService {
    /// Parameter-name hints for resolved calls whose arity matches the
    /// callee's declared parameters, restricted to `range`. Calls that do
    /// not resolve, or whose argument count disagrees with the declaration
    /// (common mid-typing), produce no hints rather than misleading ones.
    pub fn inlay_hints(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<InlayHintsResponse, ServiceError> {
        const MAX_INLAY_HINTS: usize = 200;

        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let start = snapshot
            .positions
            .offset_of(text, start_line, start_character);
        let end = snapshot.positions.offset_of(text, end_line, end_character);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };

        let tokens: Vec<&mncs_syntax::SourceToken> = snapshot
            .front_end
            .lexical
            .tokens
            .iter()
            .filter(|token| !token.kind.is_trivia())
            .collect();
        // Significant-token index by span start for paren lookup.
        let mut index_by_start = std::collections::BTreeMap::new();
        for (index, token) in tokens.iter().enumerate() {
            index_by_start.entry(token.span.start).or_insert(index);
        }

        // Callee signatures by name, cached across references so one
        // workspace scan serves the whole file.
        let mut signatures: std::collections::BTreeMap<String, Vec<SignatureParameter>> =
            std::collections::BTreeMap::new();

        let mut hints = Vec::new();
        for reference in &snapshot.symbols.references {
            if reference.kind != SymbolKind::Function {
                continue;
            }
            let at = reference.occurrence_span.start;
            if at < start || at > end {
                continue;
            }
            let params = if let Some(target) = reference.target {
                if snapshot.symbols.symbols[target].kind != SymbolKind::Function {
                    continue;
                }
                function_signature(&snapshot, target).parameters
            } else if let Some(cached) = signatures.get(&reference.target_name) {
                cached.clone()
            } else if let Some((_, owner_snapshot, index)) =
                self.resolve_function(&snapshot, uri, &reference.target_name)
            {
                let params = function_signature(&owner_snapshot, index).parameters;
                signatures.insert(reference.target_name.clone(), params.clone());
                params
            } else {
                continue;
            };
            // Argument starts: significant token after `(` then after each
            // depth-1 comma.
            let Some(paren) = index_by_start
                .get(&reference.occurrence_span.start)
                .copied()
                .and_then(|name_index| {
                    let next = name_index + 1;
                    (next < tokens.len() && tokens[next].kind == TokenKind::LeftParen)
                        .then_some(next)
                })
            else {
                continue;
            };
            let mut arg_starts = Vec::new();
            let mut depth = 0usize;
            let mut expect_arg = true;
            for token in &tokens[paren + 1..] {
                match token.kind {
                    TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::LeftBrace => {
                        if expect_arg && depth == 0 {
                            arg_starts.push(token.span.start);
                            expect_arg = false;
                        }
                        depth += 1;
                    }
                    TokenKind::RightParen | TokenKind::RightBracket | TokenKind::RightBrace => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    TokenKind::Comma if depth == 0 => {
                        expect_arg = true;
                    }
                    _ => {
                        if expect_arg && depth == 0 {
                            arg_starts.push(token.span.start);
                            expect_arg = false;
                        }
                    }
                }
            }
            if arg_starts.len() != params.len() {
                continue;
            }
            for (arg_byte, param) in arg_starts.into_iter().zip(params.iter()) {
                if arg_byte < start || arg_byte > end {
                    continue;
                }
                let position = snapshot.positions.position_of(text, arg_byte);
                hints.push(InlayHintItem {
                    line: position.line,
                    character: position.character,
                    label: format!("{}:", param.name),
                    kind: InlayHintKind::Parameter,
                });
                if hints.len() >= MAX_INLAY_HINTS {
                    break;
                }
            }
            if hints.len() >= MAX_INLAY_HINTS {
                break;
            }
        }
        hints.sort_by_key(|hint| (hint.line, hint.character));
        Ok(InlayHintsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            hints,
        })
    }
}
