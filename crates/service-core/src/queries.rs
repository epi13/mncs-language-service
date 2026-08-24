//! Protocol-neutral semantic queries over resident snapshots.
//!
//! This module defines the service's own interaction model. LSP, MCP, and any
//! future adapter translate to and from these types; none of their wire
//! schemas leak inward. Every response carries the snapshot it was computed
//! against so clients can detect staleness explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};

use mncs_model::{ObligationStatus, SemanticId};
use serde::{Deserialize, Serialize};

pub use crate::error::ServiceError;
pub use crate::indexes::SymbolKind;

use crate::analysis::DocumentAnalysis;
use crate::coords::PositionMap;
use crate::document::DocumentStore;
use crate::indexes;
use crate::render::{compute_completion, compute_semantic_tokens, render_hover_markdown};

/// Snapshot provenance attached to every response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub uri: String,
    /// Authoritative `mncs:source:artifact:<sha256>` content identity.
    pub source_identity: String,
    /// Workspace generation at analysis time.
    pub generation: u64,
    pub language_profile: String,
    /// Whether this snapshot still matches the document's current state.
    pub current: bool,
}

/// Explicit outcome status; adapters must not collapse these distinctions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseStatus {
    Answered,
    /// The capability exists but the required authoritative artifact is
    /// absent for this input (e.g., no AST because parsing failed).
    Unsupported {
        reason: String,
    },
    /// The capability ran but found no confident subject.
    Unresolved {
        reason: String,
    },
}

/// A source range in dual coordinates.
pub type Range = crate::coords::RangeInfo;

/// Projection of an indexed symbol for cross-protocol consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSummary {
    pub uri: Option<String>,
    pub name: String,
    pub kind: SymbolKind,
    /// Module-qualified semantic identity where the language defines one.
    pub identity: Option<String>,
    pub container: Option<String>,
    pub range: Range,
    pub name_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
}

/// A declaration or reference occurrence found at a position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    /// Whether the position hit a declaration site or a use site.
    pub role: OccurrenceRole,
    /// The resolved target of a reference (absent for declarations).
    pub target: Option<Box<SymbolSummary>>,
    /// For references: the occurrence's own span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence_range: Option<Range>,
    pub symbol: Box<SymbolSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceRole {
    Declaration,
    Reference,
}

// ---------------------------------------------------------------------------
// Response payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStatusResponse {
    pub workspace_root: Option<String>,
    pub generation: u64,
    pub documents: Vec<DocumentStatusEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentStatusEntry {
    pub uri: String,
    pub open: bool,
    pub buffer_version: Option<i32>,
    /// Analysis snapshot identity when one exists.
    pub analyzed_source_identity: Option<String>,
    pub analysis_current: bool,
    pub valid: bool,
    pub diagnostic_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<DiagnosticItem>,
}

/// Authoritative diagnostic projected with both coordinate systems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticItem {
    pub code: String,
    pub stage: String,
    pub severity: String,
    pub message: String,
    pub range: Range,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionQueryResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occurrences: Vec<Occurrence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<SymbolSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencesResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<ReferenceHit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceHit {
    /// Whether this hit is the declaration itself.
    pub is_declaration: bool,
    pub range: Range,
    pub name_range: Range,
    pub kind: SymbolKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbolsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<DocumentSymbolNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbolNode {
    pub summary: SymbolSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DocumentSymbolNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSymbolsResponse {
    pub status: ResponseStatus,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<WorkspaceSymbolHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSymbolHit {
    pub uri: String,
    pub summary: SymbolSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoverResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<Box<SymbolSummary>>,
    /// Canonical markdown rendering shared by all adapters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outgoing: Vec<GraphEdgeTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incoming: Vec<GraphEdgeTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeTarget {
    pub edge_kind: String,
    pub identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescribeResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<Box<SubjectDescription>>,
}

/// Machine-oriented description of one semantic subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectDescription {
    pub summary: SymbolSummary,
    pub module: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<ContractInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<ObligationInfo>,
    /// Call-graph neighbors derived from the authoritative semantic graph.
    pub calls_outgoing: usize,
    pub calls_incoming: usize,
    /// Record/finite-type structural members when applicable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractInfo {
    pub id: String,
    pub kind: String,
    pub expression: String,
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectInfo {
    pub effect_kind: String,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceInfo {
    pub property: String,
    pub verifier: String,
    /// Verbatim language-level status; never upgraded by the service.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationInfo {
    pub identity: String,
    pub subject: String,
    pub requirement: String,
    pub status: String,
    pub method: String,
    pub freshness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub name: String,
    pub kind: SymbolKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    pub identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticTokensResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    /// Absolute positions in UTF-16 coordinates plus byte offsets; adapters
    /// re-encode as required by their protocol.
    pub tokens: Vec<TokenAnnotation>,
}

/// Service-owned semantic token classes. Adapters map them onto their legends;
/// classes exist only where authoritative information justifies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenClass {
    Module,
    Function,
    Parameter,
    Variable,
    Type,
    Variant,
    Field,
    Keyword,
    Number,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAnnotation {
    pub start_line: u32,
    pub start_character: u32,
    pub length_utf16: u32,
    pub class: TokenClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<CompletionCandidate>,
    pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCandidate {
    pub label: String,
    pub class: CompletionClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Completion item classification. Deliberately distinct from `SymbolKind`:
/// keywords and builtin types are not indexed semantic subjects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionClass {
    Symbol(SymbolKind),
    Variable,
    BuiltinType,
    Keyword,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldingRangesResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<FoldRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldRange {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<Range>,
}

/// Experimental bounded semantic context packet for agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPacketResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<Box<SubjectDescription>>,
    /// Source excerpts included in the packet (bounded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excerpts: Vec<ContextExcerpt>,
    /// True only when the selection policy can justify sufficiency; the
    /// service never claims minimality.
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextExcerpt {
    pub label: String,
    pub range: Range,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

struct AnalysisSlot {
    snapshot: Arc<DocumentAnalysis>,
}

/// Resident MNCS language service core.
///
/// One instance owns workspace/document state and the analysis snapshots
/// derived from it. LSP and MCP adapters embed the same instance type; no
/// analyzer duplication exists anywhere else in this repository.
pub struct LanguageService {
    store: DocumentStore,
    analyses: RwLock<BTreeMap<String, AnalysisSlot>>,
    /// Serializes concurrent analysis of the same document without holding
    /// global locks during expensive frontend work.
    analyze_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl Default for LanguageService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LanguageService {
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self {
            store: DocumentStore::new(root),
            analyses: RwLock::new(BTreeMap::new()),
            analyze_locks: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn store(&self) -> &DocumentStore {
        &self.store
    }

    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.store.workspace_root()
    }

    /// Open/change lifecycle entry points (thin delegation).
    pub fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        self.store.did_open(uri, version, text)
    }

    pub fn did_change(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        self.store.did_change(uri, version, text)
    }

    pub fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError> {
        self.store.did_save(uri, text)
    }

    pub fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        self.store.did_close(uri)
    }

    pub fn discover_workspace(&self) -> Result<Vec<String>, ServiceError> {
        self.store.discover_workspace()
    }

    /// Current fingerprint of a document's exact content: the authoritative
    /// `SourceEnvelope` identity for that content.
    pub fn content_fingerprint(&self, uri: &str) -> Result<String, ServiceError> {
        let text = self.store.content(uri)?;
        Ok(self.store.envelope(uri, &text).identity)
    }

    /// Get (or produce) the analysis snapshot for the document's *current*
    /// content. Unchanged documents reuse the resident snapshot; changed
    /// documents are re-analyzed from scratch (correct coarse invalidation).
    ///
    /// Locking discipline: per-document mutexes serialize duplicate work, and
    /// no lock is held while the compiler frontend runs.
    pub fn snapshot(&self, uri: &str) -> Result<Arc<DocumentAnalysis>, ServiceError> {
        let fingerprint = self.content_fingerprint(uri)?;

        if let Some(existing) = self.cached_snapshot(uri, &fingerprint) {
            return Ok(existing);
        }

        // Serialize per document so concurrent editors/agents do not run the
        // frontend twice for the same state.
        let lock = Arc::clone(
            self.analyze_locks
                .lock()
                .map_err(poisoned)?
                .entry(uri.to_owned())
                .or_default(),
        );
        let _guard = lock.lock().map_err(poisoned)?;

        // Re-check after acquiring: another thread may have published.
        let fingerprint = self.content_fingerprint(uri)?;
        if let Some(existing) = self.cached_snapshot(uri, &fingerprint) {
            return Ok(existing);
        }

        let text = self.store.content(uri)?;
        let envelope = self.store.envelope(uri, &text);
        let generation = self.store.generation();
        let analysis = Arc::new(DocumentAnalysis::analyze(uri, envelope, generation));

        // Publish only if the document has not changed during analysis.
        let now_fingerprint = self.content_fingerprint(uri)?;
        if now_fingerprint == fingerprint {
            let mut analyses = self.analyses.write().map_err(poisoned)?;
            analyses.insert(
                uri.to_owned(),
                AnalysisSlot {
                    snapshot: Arc::clone(&analysis),
                },
            );
        }
        Ok(analysis)
    }

    fn cached_snapshot(&self, uri: &str, fingerprint: &str) -> Option<Arc<DocumentAnalysis>> {
        let analyses = self.analyses.read().ok()?;
        let slot = analyses.get(uri)?;
        (slot.snapshot.source_identity == fingerprint).then(|| Arc::clone(&slot.snapshot))
    }

    /// Evict cached analyses whose fingerprints no longer match; called after
    /// document mutations to bound memory.
    pub fn evict_stale_analyses(&self) -> usize {
        let mut removed = 0;
        let Ok(analyses) = self.analyses.write() else {
            return 0;
        };
        let mut keep = BTreeMap::new();
        for (uri, slot) in analyses.iter() {
            match self.store.content(uri) {
                Ok(text) => {
                    if self.store.envelope(uri, &text).identity == slot.snapshot.source_identity {
                        keep.insert(
                            uri.clone(),
                            AnalysisSlot {
                                snapshot: Arc::clone(&slot.snapshot),
                            },
                        );
                    } else {
                        removed += 1;
                    }
                }
                Err(_) => {
                    removed += 1;
                }
            }
        }
        drop(analyses);
        if let Ok(mut writable) = self.analyses.write() {
            *writable = keep;
        }
        removed
    }

    // -----------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------

    pub fn workspace_status(&self) -> WorkspaceStatusResponse {
        let mut documents = Vec::new();
        for uri in self.store.document_uris() {
            documents.push(self.document_status_entry(&uri));
        }
        documents.sort_by(|left, right| left.uri.cmp(&right.uri));
        WorkspaceStatusResponse {
            workspace_root: self
                .store
                .workspace_root()
                .map(|path| path.display().to_string()),
            generation: self.store.generation(),
            documents,
        }
    }

    fn document_status_entry(&self, uri: &str) -> DocumentStatusEntry {
        let open = self.store.is_open(uri).unwrap_or(false);
        let buffer_version = self.store.buffer_version(uri).ok().flatten();
        let (analyzed, current, valid, count) = self
            .cached_any_snapshot(uri)
            .map(|snapshot| {
                let current = self
                    .content_fingerprint(uri)
                    .map(|fingerprint| fingerprint == snapshot.source_identity)
                    .unwrap_or(false);
                (
                    Some(snapshot.source_identity.clone()),
                    current,
                    snapshot.valid(),
                    snapshot.diagnostics().len(),
                )
            })
            .unwrap_or((None, false, false, 0));
        DocumentStatusEntry {
            uri: uri.to_owned(),
            open,
            buffer_version,
            analyzed_source_identity: analyzed,
            analysis_current: current,
            valid,
            diagnostic_count: count,
        }
    }

    fn cached_any_snapshot(&self, uri: &str) -> Option<Arc<DocumentAnalysis>> {
        let analyses = self.analyses.read().ok()?;
        analyses.get(uri).map(|slot| Arc::clone(&slot.snapshot))
    }

    pub fn document_diagnostics(&self, uri: &str) -> Result<DiagnosticsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let items = render_diagnostics(&snapshot);
        Ok(DiagnosticsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            items,
        })
    }

    /// What semantic subjects exist at this position?
    pub fn subjects_at(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PositionQueryResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let byte = snapshot.positions.offset_of(text, line, character);
        let mut occurrences = Vec::new();

        if let Some(index) = snapshot.symbols.declaration_at(byte) {
            occurrences.push(Occurrence {
                role: OccurrenceRole::Declaration,
                target: None,
                occurrence_range: None,
                symbol: Box::new(summarize(uri, &snapshot, index)),
            });
        }
        for reference in snapshot.symbols.references_at(byte) {
            occurrences.push(Occurrence {
                role: OccurrenceRole::Reference,
                target: Some(Box::new(summarize(uri, &snapshot, reference.target))),
                occurrence_range: Some(
                    snapshot.positions.range_of(text, reference.occurrence_span),
                ),
                symbol: Box::new(summarize(uri, &snapshot, reference.target)),
            });
        }

        let status = if occurrences.is_empty() {
            ResponseStatus::Unresolved {
                reason: format!(
                    "no resolved semantic subject at {line}:{character}; the position may hold trivia or an unresolved identifier"
                ),
            }
        } else {
            ResponseStatus::Answered
        };
        Ok(PositionQueryResponse {
            status,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            occurrences,
        })
    }

    pub fn definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);
        let targets = self.declaration_targets(uri, &snapshot, byte);
        let status = if targets.is_empty() {
            ResponseStatus::Unresolved {
                reason: "position does not resolve to a declaration".to_owned(),
            }
        } else {
            ResponseStatus::Answered
        };
        Ok(DefinitionResponse {
            status,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            definitions: targets,
        })
    }

    fn declaration_targets(
        &self,
        uri: &str,
        snapshot: &DocumentAnalysis,
        byte: usize,
    ) -> Vec<SymbolSummary> {
        let mut targets = Vec::new();
        if let Some(index) = snapshot.symbols.declaration_at(byte) {
            targets.push(summarize(uri, snapshot, index));
        }
        for reference in snapshot.symbols.references_at(byte) {
            targets.push(summarize(uri, snapshot, reference.target));
        }
        targets.sort_by(|left, right| {
            left.range
                .start_byte
                .cmp(&right.range.start_byte)
                .then(left.name.cmp(&right.name))
        });
        targets.dedup();
        targets
    }

    pub fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let byte = snapshot.positions.offset_of(text, line, character);

        let target = self.primary_symbol_index(&snapshot, byte);
        let Some(target) = target else {
            return Ok(ReferencesResponse {
                status: ResponseStatus::Unresolved {
                    reason: "no resolved symbol at position".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                hits: Vec::new(),
            });
        };

        let mut hits = Vec::new();
        if include_declaration {
            let entry = &snapshot.symbols.symbols[target];
            hits.push(ReferenceHit {
                is_declaration: true,
                range: snapshot.positions.range_of(text, entry.full_span),
                name_range: snapshot.positions.range_of(text, entry.name_span),
                kind: entry.kind,
                name: entry.name.clone(),
                container: entry.container.clone(),
            });
        }
        for reference in snapshot.symbols.references_to(target) {
            hits.push(ReferenceHit {
                is_declaration: false,
                range: snapshot.positions.range_of(text, reference.occurrence_span),
                name_range: snapshot.positions.range_of(text, reference.occurrence_span),
                kind: reference.kind,
                name: snapshot.symbols.symbols[target].name.clone(),
                container: snapshot.symbols.symbols[target].container.clone(),
            });
        }
        hits.sort_by_key(|hit| hit.range.start_byte);
        Ok(ReferencesResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            hits,
        })
    }

    fn primary_symbol_index(&self, snapshot: &DocumentAnalysis, byte: usize) -> Option<usize> {
        snapshot
            .symbols
            .references_at(byte)
            .next()
            .map(|reference| reference.target)
            .or_else(|| snapshot.symbols.declaration_at(byte))
    }

    pub fn document_symbols(&self, uri: &str) -> Result<DocumentSymbolsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        if snapshot.front_end.ast.is_none() {
            return Ok(DocumentSymbolsResponse {
                status: ResponseStatus::Unsupported {
                    reason: "the document has no AST because parsing produced errors".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                symbols: Vec::new(),
            });
        }
        let tree = build_symbol_tree(uri, &snapshot);
        Ok(DocumentSymbolsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            symbols: tree,
        })
    }

    pub fn workspace_symbols(&self, query: &str) -> WorkspaceSymbolsResponse {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for uri in self.store.document_uris() {
            let Ok(snapshot) = self.snapshot(&uri) else {
                continue;
            };
            for index in 0..snapshot.symbols.symbols.len() {
                let entry = &snapshot.symbols.symbols[index];
                if !needle.is_empty() && !entry.name.to_lowercase().contains(&needle) {
                    continue;
                }
                hits.push(WorkspaceSymbolHit {
                    uri: uri.clone(),
                    summary: summarize(&uri, &snapshot, index),
                });
            }
        }
        hits.sort_by(|left, right| {
            left.summary
                .name
                .cmp(&right.summary.name)
                .then(left.uri.cmp(&right.uri))
        });
        hits.truncate(MAX_WORKSPACE_SYMBOLS);
        WorkspaceSymbolsResponse {
            status: ResponseStatus::Answered,
            generation: self.store.generation(),
            symbols: hits,
        }
    }

    pub fn hover(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);
        let Some(target) = self.primary_symbol_index(&snapshot, byte) else {
            return Ok(HoverResponse {
                status: ResponseStatus::Unresolved {
                    reason: "no resolvable subject under the cursor".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                subject: None,
                markdown: None,
            });
        };
        let summary = summarize(uri, &snapshot, target);
        let markdown = render_hover_markdown(&snapshot, target);
        Ok(HoverResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            subject: Some(Box::new(summary)),
            markdown: Some(markdown),
        })
    }

    pub fn describe_identity(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<DescribeResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let target = snapshot
            .symbols
            .symbols
            .iter()
            .position(|entry| {
                entry
                    .identity
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == identity)
            })
            .ok_or_else(|| ServiceError::Unresolved {
                reason: format!("identity {identity} does not belong to this snapshot"),
            })?;
        Ok(self.describe_index(uri, &snapshot, target))
    }

    pub fn describe_position(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DescribeResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);
        let Some(target) = self.primary_symbol_index(&snapshot, byte) else {
            return Err(ServiceError::Unresolved {
                reason: "no resolvable subject at position".to_owned(),
            });
        };
        Ok(self.describe_index(uri, &snapshot, target))
    }

    fn describe_index(
        &self,
        uri: &str,
        snapshot: &DocumentAnalysis,
        target: usize,
    ) -> DescribeResponse {
        let entry = &snapshot.symbols.symbols[target];
        let program = snapshot.front_end.program.as_ref();
        let mut description = SubjectDescription {
            summary: summarize(uri, snapshot, target),
            module: snapshot
                .front_end
                .ast
                .as_ref()
                .map(|ast| ast.module.text.clone())
                .unwrap_or_default(),
            contracts: Vec::new(),
            effects: Vec::new(),
            capabilities: Vec::new(),
            evidence: Vec::new(),
            obligations: Vec::new(),
            calls_outgoing: 0,
            calls_incoming: 0,
            members: Vec::new(),
        };

        if let Some(program) = program {
            let owner = if entry.kind == SymbolKind::Function {
                Some(&entry.name)
            } else {
                entry.container.as_ref()
            };
            let function = owner.and_then(|container| {
                program
                    .functions
                    .iter()
                    .find(|candidate| &candidate.name == container)
            });

            // Contracts/effects/capabilities/evidence attach at function scope.
            if let Some(function) = function {
                description.capabilities = function.capabilities.clone();
                description.effects = function
                    .effects
                    .iter()
                    .map(|effect| EffectInfo {
                        effect_kind: effect.kind.clone(),
                        capability: effect.capability.clone(),
                    })
                    .collect();
                description.contracts = function
                    .contracts
                    .iter()
                    .map(|clause| ContractInfo {
                        id: clause.id.clone(),
                        kind: contract_kind_label(&clause.kind).to_owned(),
                        expression: clause.expression.clone(),
                        identity: mncs_model::contract_id(
                            &program.module,
                            &function.name,
                            &clause.id,
                        )
                        .0,
                    })
                    .collect();
                description.evidence = function
                    .evidence
                    .iter()
                    .map(|claim| EvidenceInfo {
                        property: claim.property.clone(),
                        verifier: claim.verifier.clone(),
                        status: evidence_status_label(&claim.status).to_owned(),
                    })
                    .collect();
            }

            // Structural members for finite types and records.
            match entry.kind {
                SymbolKind::FiniteType => {
                    if let Some(declared) = program
                        .finite_types
                        .iter()
                        .find(|candidate| candidate.name == entry.name)
                    {
                        description.members = declared
                            .variants
                            .iter()
                            .map(|variant| MemberInfo {
                                name: variant.name.clone(),
                                kind: SymbolKind::FiniteVariant,
                                type_name: None,
                                identity: Some(variant.identity.0.clone()),
                            })
                            .collect();
                    }
                }
                SymbolKind::RecordType => {
                    if let Some(declared) = program
                        .record_types
                        .iter()
                        .find(|candidate| candidate.name == entry.name)
                    {
                        description.members = declared
                            .fields
                            .iter()
                            .map(|field| MemberInfo {
                                name: field.name.clone(),
                                kind: SymbolKind::RecordField,
                                type_name: Some(field.field_type.clone()),
                                identity: None,
                            })
                            .collect();
                    }
                }
                _ => {}
            }

            // Obligations whose subject resolves into this snapshot's symbols.
            let generation = program.generate_obligations();
            description.obligations = generation
                .obligations
                .iter()
                .filter(|obligation| {
                    obligation_subject_matches(obligation, entry, target, snapshot)
                })
                .map(|obligation| ObligationInfo {
                    identity: obligation.identity.0.clone(),
                    subject: obligation.subject.0.clone(),
                    requirement: obligation.requirement.0.clone(),
                    status: obligation_status_label(&obligation.status).to_owned(),
                    method: obligation.method.clone(),
                    freshness: format!("{:?}", obligation.freshness).to_lowercase(),
                    fallback: obligation.fallback.clone(),
                })
                .collect();

            // Call-graph neighborhood derived from authoritative call operations
            // (the semantic graph records Calls at operation granularity).
            if let Some(identity) = &entry.identity {
                for (caller, callee) in function_call_edges(program) {
                    if caller == *identity {
                        description.calls_outgoing += 1;
                    }
                    if callee == *identity {
                        description.calls_incoming += 1;
                    }
                }
            }
        }

        DescribeResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, snapshot)),
            subject: Some(Box::new(description)),
        }
    }

    pub fn dependencies(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        self.graph_query(uri, identity, true)
    }

    pub fn dependents(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        self.graph_query(uri, identity, false)
    }

    fn graph_query(
        &self,
        uri: &str,
        identity: &str,
        outgoing: bool,
    ) -> Result<GraphResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let Some(program) = snapshot.front_end.program.as_ref() else {
            return Ok(GraphResponse {
                status: ResponseStatus::Unsupported {
                    reason: "the semantic graph requires a valid elaborated program".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                subject_identity: Some(identity.to_owned()),
                outgoing: Vec::new(),
                incoming: Vec::new(),
            });
        };
        let wanted = SemanticId(identity.to_owned());
        let known: BTreeSet<SemanticId> = program
            .functions
            .iter()
            .map(|function| mncs_model::function_id(&program.module, &function.name))
            .collect();
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        for function in &program.functions {
            let identity = mncs_model::function_id(&program.module, &function.name);
            names.insert(identity.0, function.name.clone());
        }

        let mut out = Vec::new();
        let mut inc = Vec::new();
        for (caller, callee) in function_call_edges(program) {
            if caller == wanted && outgoing && known.contains(&callee) {
                out.push(GraphEdgeTarget {
                    edge_kind: "calls".to_owned(),
                    identity: callee.0.clone(),
                    name: names.get(&callee.0).cloned(),
                });
            }
            if callee == wanted && !outgoing && known.contains(&caller) {
                inc.push(GraphEdgeTarget {
                    edge_kind: "calls".to_owned(),
                    identity: caller.0.clone(),
                    name: names.get(&caller.0).cloned(),
                });
            }
        }
        out.sort_by(|left, right| left.identity.cmp(&right.identity));
        out.dedup();
        inc.sort_by(|left, right| left.identity.cmp(&right.identity));
        inc.dedup();

        let empty = out.is_empty() && inc.is_empty();
        Ok(GraphResponse {
            status: if empty && !known.contains(&wanted) {
                ResponseStatus::Unresolved {
                    reason: format!("identity {identity} is not a function of this snapshot"),
                }
            } else {
                ResponseStatus::Answered
            },
            snapshot: Some(snapshot_info(uri, &snapshot)),
            subject_identity: Some(identity.to_owned()),
            outgoing: out,
            incoming: inc,
        })
    }

    pub fn obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<ObligationsForUri, ServiceError> {
        let response = self.obligations_response(uri, subject_identity)?;
        Ok(response)
    }

    fn obligations_response(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<ObligationsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let Some(program) = snapshot.front_end.program.as_ref() else {
            return Ok(ObligationsResponse {
                status: ResponseStatus::Unsupported {
                    reason: "obligations require a successfully elaborated program".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                obligations: Vec::new(),
                counts: StatusCounts::default(),
            });
        };
        let generation = program.generate_obligations();
        let mut items = Vec::new();
        let mut counts = StatusCounts::default();
        for obligation in &generation.obligations {
            if let Some(wanted) = subject_identity {
                if obligation.subject.as_str() != wanted
                    && obligation.identity.as_str() != wanted
                    && !obligation
                        .dependencies
                        .iter()
                        .any(|dep| dep.as_str() == wanted)
                {
                    continue;
                }
            }
            match obligation.status {
                ObligationStatus::Pass => counts.pass += 1,
                ObligationStatus::Fail => counts.fail += 1,
                ObligationStatus::Unknown => counts.unknown += 1,
            }
            items.push(ObligationInfo {
                identity: obligation.identity.0.clone(),
                subject: obligation.subject.0.clone(),
                requirement: obligation.requirement.0.clone(),
                status: obligation_status_label(&obligation.status).to_owned(),
                method: obligation.method.clone(),
                freshness: format!("{:?}", obligation.freshness).to_lowercase(),
                fallback: obligation.fallback.clone(),
            });
        }
        Ok(ObligationsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            obligations: items,
            counts,
        })
    }

    pub fn semantic_tokens(&self, uri: &str) -> Result<SemanticTokensResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let tokens = compute_semantic_tokens(&snapshot);
        Ok(SemanticTokensResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            tokens,
        })
    }

    pub fn completion(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CompletionResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        if snapshot.front_end.ast.is_none() {
            return Ok(CompletionResponse {
                status: ResponseStatus::Unsupported {
                    reason: "completion requires a parseable document".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                items: Vec::new(),
                incomplete: false,
            });
        }
        let items = compute_completion(uri, &snapshot, line, character);
        Ok(CompletionResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            incomplete: true,
            items,
        })
    }

    pub fn folding_ranges(&self, uri: &str) -> Result<FoldingRangesResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let Some(cst) = Some(&snapshot.front_end.cst) else {
            return Ok(FoldingRangesResponse {
                status: ResponseStatus::Unsupported {
                    reason: "folding requires the concrete syntax tree".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                ranges: Vec::new(),
            });
        };
        let text = snapshot.text();
        let mut ranges = Vec::new();
        collect_fold_ranges(&snapshot.positions, text, &cst.root, &mut ranges);
        ranges.sort_by_key(|range| range.start_line);
        Ok(FoldingRangesResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            ranges,
        })
    }

    pub fn highlights(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<HighlightsResponse, ServiceError> {
        let response = self.references(uri, line, character, true)?;
        let ranges = response.hits.iter().map(|hit| hit.range).collect();
        Ok(HighlightsResponse {
            status: response.status,
            snapshot: response.snapshot,
            ranges,
        })
    }

    /// Experimental bounded context packet for agents.
    pub fn context_packet(
        &self,
        uri: &str,
        identity: &str,
        max_excerpts: usize,
    ) -> Result<ContextPacketResponse, ServiceError> {
        let described = self.describe_identity(uri, identity)?;
        let Some(subject) = described.subject else {
            return Ok(ContextPacketResponse {
                status: ResponseStatus::Unresolved {
                    reason: "subject not found".to_owned(),
                },
                snapshot: described.snapshot,
                subject: None,
                excerpts: Vec::new(),
                complete: false,
                notes: Some("cannot build a packet around an unknown subject".to_owned()),
            });
        };
        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let budget = max_excerpts.clamp(1, MAX_PACKET_EXCERPTS);

        let mut excerpts = Vec::new();
        let entry_span = subject.summary.range;
        excerpts.push(ContextExcerpt {
            label: format!("declaration {}", subject.summary.name),
            range: entry_span,
            text: safe_slice(text, entry_span.start_byte, entry_span.end_byte),
        });

        // Callees' declarations when they belong to this same document.
        if let Ok(deps) = self.dependencies(uri, identity) {
            for target in deps
                .outgoing
                .iter()
                .take(budget.saturating_sub(excerpts.len()))
            {
                if let Some(position) = snapshot.symbols.symbols.iter().position(|entry| {
                    entry
                        .identity
                        .as_ref()
                        .is_some_and(|candidate| candidate.as_str() == target.identity)
                }) {
                    let span = snapshot.symbols.symbols[position].full_span;
                    excerpts.push(ContextExcerpt {
                        label: format!("callee {}", target.name.clone().unwrap_or_default()),
                        range: snapshot.positions.range_of(text, span),
                        text: safe_slice(text, span.start, span.end),
                    });
                }
            }
        }

        // Completeness: only claimable when every outgoing call was included.
        let mut notes = None;
        let complete = match self.dependencies(uri, identity) {
            Ok(deps) if deps.outgoing.len() < excerpts.len() => true,
            Ok(deps) => {
                notes = Some(format!(
                    "{} outgoing call(s) were not included within the excerpt budget",
                    deps.outgoing
                        .len()
                        .saturating_sub(excerpts.len().saturating_sub(1))
                ));
                false
            }
            Err(_) => {
                notes = Some("dependency closure unavailable; packet may be incomplete".to_owned());
                false
            }
        };
        Ok(ContextPacketResponse {
            status: ResponseStatus::Answered,
            snapshot: described.snapshot,
            subject: Some(subject),
            excerpts,
            complete,
            notes,
        })
    }
}

/// Obligations response payload (kept separate for naming clarity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObligationsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    pub obligations: Vec<ObligationInfo>,
    pub counts: StatusCounts,
}

pub type ObligationsForUri = ObligationsResponse;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    pub pass: usize,
    pub fail: usize,
    pub unknown: usize,
}

const MAX_WORKSPACE_SYMBOLS: usize = 500;
const MAX_PACKET_EXCERPTS: usize = 12;

/// Caller→callee function edges derived from authoritative call operations
/// in elaborated bodies. The semantic graph records `Calls` at operation
/// granularity; this lifts them to the function level without inventing any
/// relationship the language did not state.
pub(crate) fn function_call_edges(program: &mncs_model::Program) -> Vec<(SemanticId, SemanticId)> {
    use mncs_model::BodyOperationKind;
    let mut edges = Vec::new();
    for function in &program.functions {
        let caller = mncs_model::function_id(&program.module, &function.name);
        let Some(body) = &function.body else { continue };
        for block in &body.blocks {
            for operation in &block.operations {
                if let BodyOperationKind::Call {
                    function: callee, ..
                } = &operation.kind
                {
                    edges.push((caller.clone(), callee.clone()));
                }
            }
        }
    }
    edges.sort();
    edges.dedup();
    edges
}

fn poisoned<T>(_: T) -> ServiceError {
    ServiceError::InvalidRequest {
        reason: "internal lock poisoned".to_owned(),
    }
}

/// Public projection helper used by the shared renderer.
pub fn summarize_public(uri: &str, snapshot: &DocumentAnalysis, index: usize) -> SymbolSummary {
    summarize(uri, snapshot, index)
}

fn snapshot_info(uri: &str, snapshot: &DocumentAnalysis) -> SnapshotInfo {
    SnapshotInfo {
        uri: uri.to_owned(),
        source_identity: snapshot.source_identity.clone(),
        generation: snapshot.generation,
        language_profile: snapshot.language_profile.clone(),
        current: true,
    }
}

pub(crate) fn summarize(uri: &str, snapshot: &DocumentAnalysis, index: usize) -> SymbolSummary {
    let entry = &snapshot.symbols.symbols[index];
    let text = snapshot.text();
    SymbolSummary {
        uri: Some(uri.to_owned()),
        name: entry.name.clone(),
        kind: entry.kind,
        identity: entry.identity.as_ref().map(|id| id.0.clone()),
        container: entry.container.clone(),
        range: snapshot.positions.range_of(text, entry.full_span),
        name_range: snapshot.positions.range_of(text, entry.name_span),
        detail: entry.detail(),
        type_name: entry.type_name.clone(),
    }
}

fn contract_kind_label(kind: &mncs_model::ContractKind) -> &'static str {
    indexes::contract_kind_label(kind)
}

fn evidence_status_label(status: &mncs_model::EvidenceStatus) -> &'static str {
    indexes::evidence_status_label(status)
}

fn obligation_status_label(status: &ObligationStatus) -> &'static str {
    indexes::obligation_status_label(status)
}

fn obligation_subject_matches(
    obligation: &mncs_model::ObligationRecord,
    entry: &crate::indexes::SymbolEntry,
    target: usize,
    snapshot: &DocumentAnalysis,
) -> bool {
    if entry
        .identity
        .as_ref()
        .is_some_and(|identity| *identity == obligation.subject)
    {
        return true;
    }
    // Function-scoped subjects: include obligations on the enclosing function
    // and on its body operations when describing that function's locals.
    if matches!(entry.kind, SymbolKind::Function) {
        if let Some(identity) = &entry.identity {
            return obligation
                .dependencies
                .iter()
                .any(|dependency| dependency == identity);
        }
    }
    // Local bindings have no obligation identity of their own; attribute
    // nothing rather than guessing.
    let _ = (target, snapshot);
    false
}

pub(crate) fn render_diagnostics(snapshot: &DocumentAnalysis) -> Vec<DiagnosticItem> {
    let text = snapshot.text();
    snapshot
        .diagnostics()
        .iter()
        .map(|diagnostic| DiagnosticItem {
            code: diagnostic.code.clone(),
            stage: format!("{:?}", diagnostic.stage).to_lowercase(),
            severity: format!("{:?}", diagnostic.severity).to_lowercase(),
            message: diagnostic.message.clone(),
            range: snapshot.positions.range_of(text, diagnostic.span),
            expected: diagnostic.expected.iter().map(format_token_kind).collect(),
            found: diagnostic.found.as_ref().map(format_token_kind),
        })
        .collect()
}

fn format_token_kind(kind: &mncs_syntax::TokenKind) -> String {
    format!("{kind:?}").to_lowercase()
}

fn build_symbol_tree(uri: &str, snapshot: &DocumentAnalysis) -> Vec<DocumentSymbolNode> {
    // Roots: module, top-level types/records, functions. Children attach via
    // parent links in the flat index.
    let count = snapshot.symbols.symbols.len();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut roots = Vec::new();
    for index in 0..count {
        match snapshot.symbols.symbols[index].parent {
            Some(parent) => children.entry(parent).or_default().push(index),
            None => roots.push(index),
        }
    }
    fn assemble(
        uri: &str,
        snapshot: &DocumentAnalysis,
        index: usize,
        children: &BTreeMap<usize, Vec<usize>>,
    ) -> DocumentSymbolNode {
        DocumentSymbolNode {
            summary: summarize(uri, snapshot, index),
            children: children
                .get(&index)
                .map(|nested| {
                    nested
                        .iter()
                        .map(|child| assemble(uri, snapshot, *child, children))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
    roots
        .into_iter()
        .map(|index| assemble(uri, snapshot, index, &children))
        .collect()
}

fn collect_fold_ranges(
    positions: &PositionMap,
    text: &str,
    node: &mncs_syntax::CstNode,
    ranges: &mut Vec<FoldRange>,
) {
    let foldable = matches!(
        node.kind,
        mncs_syntax::CstKind::FunctionDeclaration
            | mncs_syntax::CstKind::FiniteTypeDeclaration
            | mncs_syntax::CstKind::RecordTypeDeclaration
            | mncs_syntax::CstKind::Block
            | mncs_syntax::CstKind::BoundedIteration
    );
    if foldable {
        let start = positions.position_of(text, node.span.start);
        let end = positions.position_of(text, node.span.end.max(node.span.start));
        if end.line > start.line {
            ranges.push(FoldRange {
                start_line: start.line,
                end_line: end.line - 1,
            });
        }
    }
    for child in &node.children {
        collect_fold_ranges(positions, text, child, ranges);
    }
}

fn safe_slice(text: &str, start: usize, end: usize) -> String {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    text[start..end].trim().to_owned()
}
