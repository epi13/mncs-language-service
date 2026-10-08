//! Protocol-neutral semantic queries over resident snapshots.
//!
//! This module defines the service's own interaction model. LSP, MCP, and any
//! future adapter translate to and from these types; none of their wire
//! schemas leak inward. Every response carries the snapshot it was computed
//! against so clients can detect staleness explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use mncs_model::{ObligationStatus, SemanticId};
use serde::{Deserialize, Serialize};

pub use crate::error::ServiceError;
pub use crate::indexes::SymbolKind;

use crate::analysis::DocumentAnalysis;
use crate::coords::PositionMap;
use crate::debug_binding::{
    DebugBindingResolution, DebugCapabilityState, DebugCapabilityStatus, DebugSourceBinding,
    DebugSourceBindingResponse, DEBUG_SOURCE_BINDING_SCHEMA_VERSION,
};
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
    #[serde(default)]
    pub stream_identity: String,
    pub workspace_root: Option<String>,
    pub generation: u64,
    #[serde(default)]
    pub event_cursor: u64,
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
    /// Causal inner diagnostics preserved through import-boundary wrapping.
    /// Leaf spans are relative to the failing dependency's source, so each
    /// entry carries its own owning URI; `range` is present only when the
    /// owning document is resident and the span projects there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<DiagnosticRelated>,
}

/// One causal inner diagnostic with its owning location, if resolvable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticRelated {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
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
    /// URI of the document containing this occurrence.
    pub uri: String,
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

/// Upper bound on transitive-walk depth accepted from callers.
pub const MAX_TRANSITIVE_DEPTH: usize = 16;
/// Upper bound on transitive-walk nodes returned in one response.
pub const MAX_TRANSITIVE_NODES: usize = 1024;

/// One module reached by a transitive module-dependency walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitiveDepNode {
    pub uri: String,
    pub module: String,
    /// Edge count from the walk subject (direct neighbors are depth 1).
    pub depth: usize,
    /// Requested module name on the traversed import edge, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_module: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitiveDepsResponse {
    pub status: ResponseStatus,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<TransitiveDepNode>,
    /// False when the walk was truncated by depth or node bounds.
    pub complete: bool,
}

/// One function reached by a transitive caller walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitiveCallerNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub uri: String,
    pub name: String,
    /// Call-edge count from the walk subject (direct callers are depth 1).
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitiveCallersResponse {
    pub status: ResponseStatus,
    pub subject_identity: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<TransitiveCallerNode>,
    /// False when the walk was truncated by depth or node bounds.
    pub complete: bool,
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
    /// A module path on a `use` line, sourced from the discovered stdlib
    /// manifest rather than the document snapshot.
    Module,
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

/// Result of the first MNCS-native service query. `reference_counts` are the
/// Rust control result; `counts` are the independently executed MNCS result.
/// A response is `Unsupported` whenever the two disagree or the native path
/// cannot prove that its bounded input and output are valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeObligationsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<ObligationInfo>,
    pub reference_counts: StatusCounts,
    pub counts: StatusCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<crate::native_query::NativeStatusSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

struct AnalysisSlot {
    snapshot: Arc<DocumentAnalysis>,
}

const WORKSPACE_CHECKPOINT_SCHEMA: &str = "mncs.workspace-checkpoint/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceCheckpoint {
    schema_version: String,
    workspace_root: String,
    stream_identity: String,
    last_generation: u64,
    last_cursor: u64,
    /// Bounded same-stream replay window. Older checkpoints omit it and
    /// therefore retain high-water identity while requiring explicit
    /// reconciliation for cursors behind that high-water.
    #[serde(default)]
    event_history: Vec<crate::events::WorkspaceChangeEvent>,
    documents: BTreeMap<String, String>,
    /// Exact source roots selected inside the workspace. Empty in legacy
    /// checkpoints; those inherit the former whole-root behavior.
    #[serde(default)]
    discovery_roots: Vec<String>,
    /// Toolchain binding at checkpoint time. Optional so checkpoints
    /// written before toolchain binding still load; a present binding
    /// that no longer matches forces a fresh event stream.
    #[serde(default)]
    toolchain_identity: Option<crate::ambient::ToolchainIdentity>,
    /// Executable identity for the Language Service runtime itself. A build
    /// change starts a new semantic epoch even when compiler inputs match.
    #[serde(default)]
    service_build_fingerprint: Option<String>,
}

fn workspace_checkpoint_path(root: &Path) -> PathBuf {
    root.join(".mncs")
        .join("mnls-language-service.checkpoint.json")
}

fn load_workspace_checkpoint(
    path: &Path,
    root: &Path,
) -> Result<Option<WorkspaceCheckpoint>, ServiceError> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ServiceError::InvalidRequest {
                reason: format!("could not read workspace checkpoint: {error}"),
            });
        }
    };
    let checkpoint: WorkspaceCheckpoint =
        serde_json::from_slice(&raw).map_err(|error| ServiceError::InvalidRequest {
            reason: format!("workspace checkpoint is malformed: {error}"),
        })?;
    if checkpoint.schema_version != WORKSPACE_CHECKPOINT_SCHEMA {
        return Err(ServiceError::InvalidRequest {
            reason: format!(
                "workspace checkpoint schema is unsupported: {}",
                checkpoint.schema_version
            ),
        });
    }
    if checkpoint.workspace_root != root.display().to_string()
        || checkpoint.stream_identity.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(checkpoint))
}

/// Resident MNCS language service core.
///
/// One instance owns workspace/document state and the analysis snapshots
/// derived from it. LSP and MCP adapters embed the same instance type; no
/// analyzer duplication exists anywhere else in this repository.
pub struct LanguageService {
    pub(crate) store: DocumentStore,
    pub(crate) events: Arc<crate::events::EventHub>,
    analyses: Arc<RwLock<BTreeMap<String, AnalysisSlot>>>,
    /// Serializes concurrent analysis of the same document without holding
    /// global locks during expensive frontend work.
    analyze_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// Resident workspace indexes (identity/declaration/reference/dependency)
    /// published alongside snapshots. Lock order is analyses -> index ->
    /// store; the per-document analyze mutex stays outermost.
    pub(crate) workspace_index: Arc<RwLock<crate::workspace_index::WorkspaceIndex>>,
    pub(crate) stats: crate::workspace_index::ServiceStats,
    /// Store content version covered by the last fully successful
    /// workspace-wide ensure. Entries without external dependencies are
    /// provably current while this matches, so warm cross-document queries
    /// skip per-document validation entirely.
    last_ensured_version: AtomicU64,
    pub(crate) native_kernel: RwLock<Option<Arc<crate::native_query::NativeQueryKernel>>>,
    pub(crate) filter_kernel: RwLock<Option<Arc<crate::native_filter::NativeFilterKernel>>>,
    pub(crate) capsule_kernel: RwLock<Option<Arc<crate::ambient::CapsuleKernel>>>,
    checkpoint: Mutex<Option<WorkspaceCheckpoint>>,
    instance_id: String,
}

impl Default for LanguageService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LanguageService {
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        let analyses = Arc::new(RwLock::new(BTreeMap::new()));
        let workspace_index = Arc::new(RwLock::new(
            crate::workspace_index::WorkspaceIndex::default(),
        ));
        let store = DocumentStore::new(root);
        // Every committed content change invalidates the changed document
        // plus its transitive importers: analyses embed their dependencies'
        // content at elaboration time, so anything downstream must
        // re-analyze on next access. Unaffected documents keep their state.
        store.set_change_callback({
            let analyses = Arc::clone(&analyses);
            let workspace_index = Arc::clone(&workspace_index);
            Arc::new(move |uri: &str| {
                invalidate_transitive_importers(&analyses, &workspace_index, uri);
            })
        });
        Self {
            store,
            events: Arc::new(crate::events::EventHub::default()),
            analyses,
            analyze_locks: Mutex::new(BTreeMap::new()),
            workspace_index,
            stats: crate::workspace_index::ServiceStats::default(),
            last_ensured_version: AtomicU64::new(u64::MAX),
            native_kernel: RwLock::new(None),
            filter_kernel: RwLock::new(None),
            capsule_kernel: RwLock::new(None),
            checkpoint: Mutex::new(None),
            instance_id: crate::ambient::new_instance_id(),
        }
    }

    /// Process-bound instance identity, distinct across restarts even when
    /// the event stream is restored from the durable checkpoint.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Compact observation of the durable checkpoint for ambient status.
    pub(crate) fn checkpoint_observation(&self) -> Option<crate::ambient::CheckpointObservation> {
        let checkpoint = self.checkpoint.lock().ok()?.clone()?;
        let current = crate::ambient::ToolchainIdentity::current();
        Some(crate::ambient::CheckpointObservation {
            stream_identity: checkpoint.stream_identity,
            last_generation: checkpoint.last_generation,
            last_cursor: checkpoint.last_cursor,
            toolchain_matches_current: checkpoint
                .toolchain_identity
                .as_ref()
                .map(|bound| bound == &current)
                .unwrap_or(true),
        })
    }

    pub fn store(&self) -> &DocumentStore {
        &self.store
    }

    pub fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.store.workspace_root()
    }

    pub fn events(&self) -> Arc<crate::events::EventHub> {
        Arc::clone(&self.events)
    }

    pub fn poll_events(
        &self,
        after_cursor: u64,
        max_events: usize,
    ) -> crate::events::WorkspaceEventCursor {
        self.events.poll(after_cursor, max_events)
    }

    pub fn poll_events_for(
        &self,
        stream_identity: Option<&str>,
        after_cursor: u64,
        max_events: usize,
    ) -> crate::events::WorkspaceEventCursor {
        self.events
            .poll_for(stream_identity, after_cursor, max_events)
    }

    pub fn refresh_workspace(&self) -> Result<Vec<u64>, ServiceError> {
        let new_documents = self.store.discover_new_documents()?;
        let mut published = Vec::new();
        for uri in new_documents {
            if let Some(generation) = self.store.load_new_disk(&uri)? {
                if let Some(cursor) = self.observe_change(
                    &uri,
                    generation.saturating_sub(1),
                    generation,
                    None,
                    false,
                    true,
                )? {
                    published.push(cursor);
                }
            }
        }
        let mut before_by_uri = BTreeMap::<String, Arc<DocumentAnalysis>>::new();
        let changed = self.store.refresh_disk_with(|uri| {
            let snapshot = self
                .cached_snapshot_if_current(uri)
                .or_else(|| self.snapshot(uri).ok());
            if let Some(snapshot) = snapshot {
                before_by_uri.insert(uri.to_owned(), snapshot);
                true
            } else {
                false
            }
        })?;
        for (uri, generation, removed) in changed {
            let before = before_by_uri.get(&uri).cloned();
            let cursor = if removed {
                match before.as_deref() {
                    Some(snapshot) => self.observe_removal(
                        &uri,
                        generation.saturating_sub(1),
                        generation,
                        snapshot,
                    )?,
                    None => None,
                }
            } else {
                self.observe_change(
                    &uri,
                    generation.saturating_sub(1),
                    generation,
                    before,
                    false,
                    true,
                )?
            };
            if let Some(cursor) = cursor {
                published.push(cursor);
            }
        }
        Ok(published)
    }

    fn observe_change(
        &self,
        uri: &str,
        replaced_generation: u64,
        expected_generation: u64,
        before: Option<Arc<DocumentAnalysis>>,
        reconciled: bool,
        persist_checkpoint: bool,
    ) -> Result<Option<u64>, ServiceError> {
        let snapshot_started = std::time::Instant::now();
        let source_bytes = self.store.content(uri).map(|text| text.len()).unwrap_or(0);
        crate::startup_profile::emit(
            "changed_document_snapshot_started",
            serde_json::json!({
                "uri": uri,
                "source_bytes": source_bytes,
                "expected_generation": expected_generation,
                "reconciled": reconciled,
            }),
        );
        let after = self.snapshot(uri)?;
        crate::startup_profile::emit(
            "changed_document_snapshot",
            serde_json::json!({
                "uri": uri,
                "source_bytes": source_bytes,
                "source_identity": after.source_identity.as_str(),
                "generation": after.generation,
                "reconciled": reconciled,
                "elapsed_us": crate::startup_profile::elapsed_us(snapshot_started),
            }),
        );
        // ReferenceCompiler is synchronous and cannot be interrupted.  The
        // generation check is therefore the authoritative stale-work guard:
        // an analysis that finished after a newer edit is never published as
        // a current event or evidence.
        if self.store.generation() != expected_generation || after.generation != expected_generation
        {
            return Ok(None);
        }
        let event = crate::events::project_change(
            uri,
            replaced_generation,
            expected_generation,
            before.as_deref(),
            &after,
            reconciled,
        );
        let cursor = self.events.push(event);
        self.evict_stale_analyses();
        if persist_checkpoint {
            self.persist_checkpoint()?;
        }
        Ok(Some(cursor))
    }

    fn observe_removal(
        &self,
        uri: &str,
        replaced_generation: u64,
        expected_generation: u64,
        before: &DocumentAnalysis,
    ) -> Result<Option<u64>, ServiceError> {
        if self.store.generation() != expected_generation {
            return Ok(None);
        }
        let supersedes_through_cursor = self.events.latest_cursor_for_uri(uri);
        let event = crate::events::project_removal(
            uri,
            replaced_generation,
            expected_generation,
            before,
            supersedes_through_cursor,
        );
        let cursor = self.events.push(event);
        self.evict_stale_analyses();
        self.persist_checkpoint()?;
        Ok(Some(cursor))
    }

    fn begin_change(&self, uri: &str) -> (u64, Option<Arc<DocumentAnalysis>>) {
        let replaced_generation = self.store.generation();
        let before = self.cached_snapshot_if_current(uri).or_else(|| {
            self.snapshot(uri).ok().filter(|snapshot| {
                self.content_fingerprint(uri).ok().as_deref()
                    == Some(snapshot.source_identity.as_str())
            })
        });
        (replaced_generation, before)
    }

    /// Open/change lifecycle entry points (thin delegation).
    pub fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        let (replaced_generation, before) = self.begin_change(uri);
        let generation = self.store.did_open(uri, version, text)?;
        let _ = self.observe_change(uri, replaced_generation, generation, before, false, true)?;
        Ok(generation)
    }

    pub fn did_change(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        self.did_open(uri, version, text)
    }

    pub fn did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<crate::edits::TextChange>,
    ) -> Result<u64, ServiceError> {
        let (replaced_generation, before) = self.begin_change(uri);
        let generation = self.store.did_change_incremental(uri, version, changes)?;
        let _ = self.observe_change(uri, replaced_generation, generation, before, false, true)?;
        Ok(generation)
    }

    pub fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError> {
        let (replaced_generation, before) = self.begin_change(uri);
        let generation = self.store.did_save(uri, text)?;
        if generation == replaced_generation {
            return Ok(generation);
        }
        let _ = self.observe_change(uri, replaced_generation, generation, before, false, true)?;
        Ok(generation)
    }

    pub fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        let (replaced_generation, before) = self.begin_change(uri);
        let content = self.store.did_close(uri)?;
        if content.is_some() {
            let generation = self.store.generation();
            let _ =
                self.observe_change(uri, replaced_generation, generation, before, false, true)?;
        } else {
            self.evict_stale_analyses();
        }
        Ok(content)
    }

    pub fn discover_workspace(&self) -> Result<Vec<String>, ServiceError> {
        self.store.discover_workspace()
    }

    /// Attach (or replace) the workspace root after construction and run an
    /// initial discovery scan. Used by adapters whose protocol supplies the
    /// root during initialization rather than at process start.
    pub fn configure_root(
        &self,
        root: Option<std::path::PathBuf>,
    ) -> Result<Vec<String>, ServiceError> {
        self.configure_root_with_discovery_roots(root, None)
    }

    /// Configure a workspace while constraining disk discovery to exact
    /// provider-selected roots. This keeps one resident service bound to an
    /// Environment composition instead of silently scanning every sibling
    /// checkout under a broad filesystem root.
    pub fn configure_root_with_discovery_roots(
        &self,
        root: Option<std::path::PathBuf>,
        discovery_roots: Option<Vec<std::path::PathBuf>>,
    ) -> Result<Vec<String>, ServiceError> {
        let configure_started = std::time::Instant::now();
        let Some(root) = root else {
            self.store.set_root(None);
            if let Ok(mut checkpoint) = self.checkpoint.lock() {
                *checkpoint = None;
            }
            return Ok(Vec::new());
        };
        let root = root
            .canonicalize()
            .map_err(|error| ServiceError::WorkspaceUnavailable {
                path: format!("{}: {error}", root.display()),
            })?;
        let requested_roots = discovery_roots.unwrap_or_else(|| vec![root.clone()]);
        if requested_roots.is_empty() {
            return Err(ServiceError::WorkspaceUnavailable {
                path: "selected source-root set is empty".to_owned(),
            });
        }
        let mut normalized_roots = Vec::with_capacity(requested_roots.len());
        for requested in requested_roots {
            if !requested.is_absolute() {
                return Err(ServiceError::WorkspaceUnavailable {
                    path: format!(
                        "selected source root is not absolute: {}",
                        requested.display()
                    ),
                });
            }
            let selected =
                requested
                    .canonicalize()
                    .map_err(|error| ServiceError::WorkspaceUnavailable {
                        path: format!("{}: {error}", requested.display()),
                    })?;
            if !selected.starts_with(&root) || !selected.is_dir() {
                return Err(ServiceError::WorkspaceUnavailable {
                    path: format!(
                        "selected source root escapes or is not a directory: {}",
                        selected.display()
                    ),
                });
            }
            normalized_roots.push(selected);
        }
        normalized_roots.sort();
        normalized_roots.dedup();
        let already_configured = self.store.workspace_root().as_deref() == Some(root.as_path())
            && self.store.discovery_roots() == normalized_roots
            && self
                .checkpoint
                .lock()
                .ok()
                .and_then(|checkpoint| checkpoint.as_ref().map(|_| ()))
                .is_some();
        if already_configured {
            return Ok(self.store.document_uris());
        }

        self.store.set_root(Some(root.clone()));
        self.store
            .set_discovery_roots(Some(normalized_roots.clone()));
        let checkpoint_path = workspace_checkpoint_path(&root);
        let checkpoint_load_started = std::time::Instant::now();
        let checkpoint = load_workspace_checkpoint(&checkpoint_path, &root)?;
        crate::startup_profile::emit(
            "checkpoint_load",
            serde_json::json!({
                "exists": checkpoint_path.exists(),
                "bytes": fs::metadata(&checkpoint_path).map(|metadata| metadata.len()).unwrap_or(0),
                "elapsed_us": crate::startup_profile::elapsed_us(checkpoint_load_started),
            }),
        );
        let toolchain_started = std::time::Instant::now();
        let current_toolchain = crate::ambient::ToolchainIdentity::current();
        crate::startup_profile::emit(
            "toolchain_identity",
            serde_json::json!({
                "digest": current_toolchain.digest(),
                "elapsed_us": crate::startup_profile::elapsed_us(toolchain_started),
            }),
        );
        let build_fingerprint_started = std::time::Instant::now();
        let (_, current_service_build) = crate::ambient::build_fingerprint();
        crate::startup_profile::emit(
            "service_build_fingerprint",
            serde_json::json!({
                "fingerprint": current_service_build.as_str(),
                "elapsed_us": crate::startup_profile::elapsed_us(build_fingerprint_started),
            }),
        );
        let normalized_root_ids: Vec<String> = normalized_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        // A toolchain change invalidates stream continuity: identical bytes
        // under a different toolchain may mean different semantics, so the
        // restored cursor must not resume silently. Generation continuity
        // is kept (it counts workspace changes, not meanings) while the
        // stream starts a fresh epoch consumers must reconcile against.
        let toolchain_changed = checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.toolchain_identity.as_ref())
            .map(|bound| bound != &current_toolchain)
            .unwrap_or(false);
        let prior_roots = checkpoint.as_ref().map(|checkpoint| {
            if checkpoint.discovery_roots.is_empty() {
                vec![root.display().to_string()]
            } else {
                checkpoint.discovery_roots.clone()
            }
        });
        let roots_changed = prior_roots
            .as_ref()
            .is_some_and(|previous| previous != &normalized_root_ids);
        let service_build_changed = checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.service_build_fingerprint.as_deref() != Some(current_service_build.as_str())
        });
        let continuity_changed = toolchain_changed || roots_changed || service_build_changed;
        if let Some(checkpoint) = checkpoint.as_ref() {
            self.store
                .restore_generation_at_least(checkpoint.last_generation);
            if !continuity_changed {
                self.events.restore_checkpoint(
                    checkpoint.stream_identity.clone(),
                    checkpoint.last_cursor,
                    checkpoint.event_history.clone(),
                );
            }
        }
        if let Ok(mut writable) = self.checkpoint.lock() {
            *writable = Some(checkpoint.clone().unwrap_or_else(|| WorkspaceCheckpoint {
                schema_version: WORKSPACE_CHECKPOINT_SCHEMA.to_owned(),
                workspace_root: root.display().to_string(),
                stream_identity: self.events.stream_identity(),
                last_generation: self.store.generation(),
                last_cursor: self.events.current_cursor(),
                event_history: Vec::new(),
                documents: BTreeMap::new(),
                discovery_roots: normalized_root_ids.clone(),
                toolchain_identity: Some(current_toolchain.clone()),
                service_build_fingerprint: Some(current_service_build.clone()),
            }));
            if continuity_changed {
                if let Some(writable) = writable.as_mut() {
                    writable.stream_identity = self.events.stream_identity();
                    writable.last_cursor = self.events.current_cursor();
                    writable.event_history.clear();
                    writable.toolchain_identity = Some(current_toolchain.clone());
                    writable.service_build_fingerprint = Some(current_service_build.clone());
                }
            }
            if let Some(writable) = writable.as_mut() {
                writable.discovery_roots = normalized_root_ids;
            }
        }

        let changed = self
            .store
            .reconcile_checkpoint(checkpoint.as_ref().map(|value| &value.documents))?;
        // Reconciliation assigns one generation per changed file, but all
        // resulting snapshots describe the final workspace generation. If
        // each item checked its individual generation here, the first N-1
        // events would be discarded as stale after the last file advanced
        // the shared generation counter.
        let reconciled_generation = self.store.generation();
        for (uri, generation) in changed {
            let _ = self.observe_change(
                &uri,
                generation.saturating_sub(1),
                reconciled_generation,
                None,
                true,
                false,
            )?;
        }
        self.persist_checkpoint()?;
        crate::startup_profile::emit(
            "workspace_configured",
            serde_json::json!({
                "documents": self.store.document_uris().len(),
                "elapsed_us": crate::startup_profile::elapsed_us(configure_started),
            }),
        );
        Ok(self.store.document_uris())
    }

    fn checkpoint_identities(&self) -> BTreeMap<String, String> {
        self.store
            .document_uris()
            .into_iter()
            .filter_map(|uri| {
                let text = self.store.content(&uri).ok()?;
                Some((uri.clone(), self.store.envelope(&uri, &text).identity))
            })
            .collect()
    }

    fn persist_checkpoint(&self) -> Result<(), ServiceError> {
        let persist_started = std::time::Instant::now();
        let Some(root) = self.store.workspace_root() else {
            return Ok(());
        };
        let path = workspace_checkpoint_path(&root);
        let documents = self.checkpoint_identities();
        let document_count = documents.len();
        let mut checkpoint = self
            .checkpoint
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace checkpoint state was poisoned".to_owned(),
            })?
            .clone()
            .unwrap_or_else(|| WorkspaceCheckpoint {
                schema_version: WORKSPACE_CHECKPOINT_SCHEMA.to_owned(),
                workspace_root: root.display().to_string(),
                stream_identity: self.events.stream_identity(),
                last_generation: 0,
                last_cursor: 0,
                event_history: Vec::new(),
                documents: BTreeMap::new(),
                discovery_roots: self
                    .store
                    .discovery_roots()
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                toolchain_identity: Some(crate::ambient::ToolchainIdentity::current()),
                service_build_fingerprint: Some(crate::ambient::build_fingerprint().1),
            });
        checkpoint.last_generation = self.store.generation();
        let (stream_identity, last_cursor, event_history) = self.events.checkpoint_snapshot();
        checkpoint.stream_identity = stream_identity;
        checkpoint.last_cursor = last_cursor;
        checkpoint.event_history = event_history;
        checkpoint.documents = documents;
        checkpoint.discovery_roots = self
            .store
            .discovery_roots()
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        checkpoint.toolchain_identity = Some(crate::ambient::ToolchainIdentity::current());
        checkpoint.service_build_fingerprint = Some(crate::ambient::build_fingerprint().1);
        let raw = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            ServiceError::InvalidRequest {
                reason: format!("could not encode workspace checkpoint: {error}"),
            }
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| ServiceError::InvalidRequest {
                reason: format!("could not create checkpoint directory: {error}"),
            })?;
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let temporary = path.with_extension(format!("json.{}-{nonce}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| ServiceError::InvalidRequest {
                reason: format!("could not open workspace checkpoint: {error}"),
            })?;
        file.write_all(&raw)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| ServiceError::InvalidRequest {
                reason: format!("could not persist workspace checkpoint: {error}"),
            })?;
        fs::rename(&temporary, &path).map_err(|error| ServiceError::InvalidRequest {
            reason: format!("could not publish workspace checkpoint: {error}"),
        })?;
        if let Ok(mut writable) = self.checkpoint.lock() {
            *writable = Some(checkpoint);
        }
        crate::startup_profile::emit(
            "checkpoint_persisted",
            serde_json::json!({
                "documents": document_count,
                "encoded_bytes": raw.len(),
                "elapsed_us": crate::startup_profile::elapsed_us(persist_started),
            }),
        );
        Ok(())
    }

    /// Current fingerprint of a document's exact content: the authoritative
    /// `SourceEnvelope` identity for that content, sealed when the content
    /// was stored (no re-hashing on warm queries).
    pub fn content_fingerprint(&self, uri: &str) -> Result<String, ServiceError> {
        self.store.content_identity(uri)
    }

    /// Machine-readable query-execution counters for tests, Debug, and
    /// Doctor visibility into resident-state behavior.
    pub fn service_stats(&self) -> crate::workspace_index::ServiceStatsSnapshot {
        let mut snapshot = self.stats.snapshot();
        if let Ok(index) = self.workspace_index.read() {
            snapshot.indexed_documents = index.len();
            snapshot.indexed_occurrences = index.occurrence_count();
        }
        snapshot
    }

    /// Reset query-execution counters (index contents are untouched).
    pub fn reset_service_stats(&self) {
        self.stats.reset();
    }

    /// Exact resident content for adapters that need to materialize a
    /// protocol edit.  The service remains the sole owner of the source
    /// state; clients receive a bounded projection rather than the store.
    pub fn content(&self, uri: &str) -> Result<String, ServiceError> {
        Ok((*self.store.content(uri)?).clone())
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
            self.stats.record_snapshot_hit();
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
            self.stats.record_snapshot_hit();
            return Ok(existing);
        }

        let text = self.store.content(uri)?;
        let envelope = self.store.envelope(uri, &text);
        let generation = self.store.generation();
        let resolver = crate::modules::StoreResolver::new(&self.store);
        self.stats.record_frontend_run();
        let analysis = Arc::new(DocumentAnalysis::analyze_with_resolver(
            uri,
            envelope,
            generation,
            &self.store,
            &resolver,
        ));

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
            drop(analyses);
            // The workspace index entry is derived from the published
            // snapshot, so indexed queries observe exactly what per-document
            // snapshot storms would find.
            self.publish_workspace_entry(uri, &analysis);
        }
        self.stats.record_snapshot_miss();
        Ok(analysis)
    }

    /// Publish (or refresh) the workspace index entry for a resident
    /// snapshot. Entries for other documents are untouched.
    pub(crate) fn publish_workspace_entry(&self, uri: &str, snapshot: &Arc<DocumentAnalysis>) {
        let entry =
            crate::workspace_index::WorkspaceEntry::build(&self.store, uri, snapshot, summarize);
        if let Ok(mut index) = self.workspace_index.write() {
            index.publish(uri.to_owned(), entry);
            self.stats.record_index_publish();
        }
    }

    /// Ensure the workspace index entry for `uri` is current, repairing it
    /// through the ordinary snapshot funnel when stale or missing. Returns
    /// whether a usable entry is resident afterwards.
    pub(crate) fn ensure_workspace_entry(&self, uri: &str) -> bool {
        let needs_repair = match self.workspace_index.read() {
            Ok(index) => match index.entry(uri) {
                Some(entry) => !entry.is_current(&self.store, uri),
                None => true,
            },
            Err(_) => true,
        };
        if !needs_repair {
            self.stats.record_index_query();
            return true;
        }
        self.stats.record_index_repair();
        match self.snapshot(uri) {
            Ok(_) => self.workspace_index.read().ok().is_some_and(|index| {
                index
                    .entry(uri)
                    .is_some_and(|entry| entry.is_current(&self.store, uri))
            }),
            Err(_) => {
                if let Ok(mut index) = self.workspace_index.write() {
                    index.remove(uri);
                }
                false
            }
        }
    }

    /// Ensure entries for every known document. Returns URIs that could not
    /// be ensured (unknown or unreachable documents), in URI order.
    pub(crate) fn ensure_workspace_entries(&self) -> Vec<String> {
        // Fast path: nothing in the store changed since the last fully
        // successful ensure, so every entry without external dependencies
        // is still current. External-file importers always re-validate
        // because library files change without store mutations.
        let version = self.store.content_version();
        if self.last_ensured_version.load(Ordering::Relaxed) == version {
            let external = self
                .workspace_index
                .read()
                .ok()
                .map(|index| index.external_dep_uris())
                .unwrap_or_default();
            let mut failed = Vec::new();
            for uri in external {
                if !self.ensure_workspace_entry(&uri) {
                    failed.push(uri);
                }
            }
            return failed;
        }
        let mut uris = self.store.document_uris();
        uris.sort();
        let mut failed = Vec::new();
        for uri in uris {
            if !self.ensure_workspace_entry(&uri) {
                failed.push(uri);
            }
        }
        // Record only when no store mutation interleaved: a mid-ensure edit
        // may have invalidated entries validated before it landed.
        if failed.is_empty() && self.store.content_version() == version {
            self.last_ensured_version.store(version, Ordering::Relaxed);
        }
        failed
    }

    fn cached_snapshot(&self, uri: &str, fingerprint: &str) -> Option<Arc<DocumentAnalysis>> {
        let analyses = self.analyses.read().ok()?;
        let slot = analyses.get(uri)?;
        if slot.snapshot.source_identity != fingerprint {
            return None;
        }
        // Multi-module staleness: a snapshot is current only when every
        // direct dependency's identity is still what was recorded. This
        // keeps importers honest when a dependency edits without the
        // importer changing.
        if !slot.snapshot.dependencies.is_empty() {
            let Ok(text) = self.store.content(uri) else {
                return None;
            };
            let now = crate::modules::DependencyFingerprints::collect(&self.store, &text);
            if now.modules != slot.snapshot.dependencies.modules {
                return None;
            }
        }
        Some(Arc::clone(&slot.snapshot))
    }

    /// Evict cached analyses whose fingerprints no longer match; called after
    /// document mutations to bound memory. Workspace index entries for
    /// evicted documents are dropped with them.
    pub fn evict_stale_analyses(&self) -> usize {
        let stale: Vec<String> = {
            let Ok(analyses) = self.analyses.read() else {
                return 0;
            };
            analyses
                .iter()
                .filter_map(|(uri, slot)| {
                    let current = self.store.content_identity(uri).ok();
                    if current.as_deref() == Some(slot.snapshot.source_identity.as_str()) {
                        None
                    } else {
                        Some(uri.clone())
                    }
                })
                .collect()
        };
        if stale.is_empty() {
            return 0;
        }
        // Re-validate under the write lock so a snapshot published after the
        // scan is never evicted.
        let mut removed = 0;
        let mut evicted = Vec::new();
        if let Ok(mut analyses) = self.analyses.write() {
            for uri in &stale {
                let still_stale = analyses.get(uri).is_some_and(|slot| {
                    self.store.content_identity(uri).ok().as_deref()
                        != Some(slot.snapshot.source_identity.as_str())
                });
                if still_stale && analyses.remove(uri).is_some() {
                    removed += 1;
                    evicted.push(uri.clone());
                }
            }
        }
        if !evicted.is_empty() {
            if let Ok(mut index) = self.workspace_index.write() {
                for uri in evicted {
                    index.remove(&uri);
                }
            }
        }
        removed
    }

    // -----------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------

    pub fn workspace_status(&self) -> Result<WorkspaceStatusResponse, ServiceError> {
        let mut documents = Vec::new();
        for uri in self.store.document_uris() {
            documents.push(self.document_status_entry(&uri));
        }
        documents.sort_by(|left, right| left.uri.cmp(&right.uri));
        Ok(WorkspaceStatusResponse {
            stream_identity: self.events.stream_identity(),
            workspace_root: self.store.workspace_root_path(),
            generation: self.store.generation(),
            event_cursor: self.events.current_cursor(),
            documents,
        })
    }

    /// Query the single language capability projection generated by
    /// `mncs-language`. The service supplies bounded filtering and deltas but
    /// does not maintain a second language reference.
    pub fn language_capabilities(
        &self,
        topic: Option<&str>,
        symbol: Option<&str>,
        profile: Option<&str>,
        delta_from: Option<&str>,
        known_identity: Option<&str>,
        max_items: usize,
    ) -> Result<crate::language_knowledge::LanguageCapabilitiesResponse, ServiceError> {
        crate::language_knowledge::query(
            self.workspace_root().as_deref(),
            topic,
            symbol,
            profile,
            delta_from,
            known_identity,
            max_items,
        )
    }

    /// Compose one bounded repository/family preflight.  The individual
    /// language, Commons, and Atlas facts remain owned by their source
    /// repositories; this service owns only the query envelope and bounds.
    pub fn family_agent_context(
        &self,
        repository: Option<&str>,
        topic: Option<&str>,
        symbol: Option<&str>,
        known_language_identity: Option<&str>,
        known_architecture_identity: Option<&str>,
        max_items: usize,
    ) -> Result<crate::family_context::FamilyAgentContextResponse, ServiceError> {
        crate::family_context::query(
            self.workspace_root().as_deref(),
            repository,
            topic,
            symbol,
            known_language_identity,
            known_architecture_identity,
            max_items,
        )
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

    /// Return a cached analysis only when its source and dependencies are
    /// current. Health/status projections use this to report measured state
    /// without turning a readiness probe into a workspace-wide compile.
    pub(crate) fn cached_snapshot_if_current(&self, uri: &str) -> Option<Arc<DocumentAnalysis>> {
        let fingerprint = self.store.content_identity(uri).ok()?;
        self.cached_snapshot(uri, &fingerprint)
    }

    pub fn document_diagnostics(&self, uri: &str) -> Result<DiagnosticsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let items = render_diagnostics(&snapshot, &self.store);
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
            let Some(target) = reference.target else {
                continue;
            };
            occurrences.push(Occurrence {
                role: OccurrenceRole::Reference,
                target: Some(Box::new(summarize(uri, &snapshot, target))),
                occurrence_range: Some(
                    snapshot.positions.range_of(text, reference.occurrence_span),
                ),
                symbol: Box::new(summarize(uri, &snapshot, target)),
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

    /// Project one compiler-owned source subject into the shared debugger
    /// vocabulary. `identity` may be a function, test declaration, test case,
    /// or module identity. Alternatively, provide a zero-based source
    /// position. Operation identities are resolved through the compiler-owned
    /// execution source map; live suspension and runtime failure control are
    /// separate capabilities.
    pub fn debug_source_binding(
        &self,
        uri: &str,
        identity: Option<&str>,
        line: Option<u32>,
        character: Option<u32>,
    ) -> Result<DebugSourceBindingResponse, ServiceError> {
        let has_identity = identity.is_some();
        let has_position = line.is_some() || character.is_some();
        if has_identity == has_position || (has_position && (line.is_none() || character.is_none()))
        {
            return Err(ServiceError::InvalidRequest {
                reason: "debug_source_binding requires exactly one identity or line+character"
                    .to_owned(),
            });
        }

        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let inventory = snapshot.front_end.test_inventory.as_ref();
        let execution_source_map = snapshot.front_end.execution_source_map.as_ref();
        let operation = identity.and_then(|identity| {
            execution_source_map.and_then(|source_map| {
                source_map
                    .operations
                    .iter()
                    .find(|candidate| candidate.identity.as_str() == identity)
            })
        });
        let (test, target) = if let Some(identity) = identity {
            let test = inventory.and_then(|inventory| {
                inventory.tests.iter().find(|entry| {
                    entry.test_case_identity.as_str() == identity
                        || entry.declaration_identity.as_str() == identity
                        || entry.function_identity.as_str() == identity
                })
            });
            let mut target = snapshot.symbols.symbols.iter().position(|entry| {
                entry
                    .identity
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == identity)
            });
            let module = module_name(&snapshot);
            if target.is_none() && identity == mncs_model::module_id(&module).as_str() {
                target = snapshot
                    .symbols
                    .symbols
                    .iter()
                    .position(|entry| entry.kind == SymbolKind::Module);
            }
            if target.is_none() {
                if let Some(operation) = operation {
                    target = snapshot.symbols.symbols.iter().position(|entry| {
                        entry.identity.as_ref() == Some(&operation.function_identity)
                    });
                }
            }
            (test, target)
        } else {
            let byte = snapshot.positions.offset_of(
                text,
                line.expect("validated line"),
                character.expect("validated character"),
            );
            let target = self.primary_symbol_index(&snapshot, byte);
            let test = inventory.and_then(|inventory| {
                inventory
                    .tests
                    .iter()
                    .find(|entry| entry.source_span.start <= byte && byte <= entry.source_span.end)
            });
            (test, target)
        };

        let target_entry = target.map(|index| &snapshot.symbols.symbols[index]);
        if target_entry.is_none() && test.is_none() && operation.is_none() {
            return Err(ServiceError::Unresolved {
                reason: "identity or position does not resolve to a compiler-owned source subject"
                    .to_owned(),
            });
        }

        let module = module_name(&snapshot);
        let function_name = operation
            .and_then(|entry| {
                execution_source_map.and_then(|source_map| {
                    source_map
                        .functions
                        .iter()
                        .find(|function| function.identity == entry.function_identity)
                        .map(|function| function.name.clone())
                })
            })
            .or_else(|| test.map(|entry| entry.name.clone()))
            .or_else(|| {
                target_entry
                    .filter(|entry| entry.kind == SymbolKind::Function)
                    .map(|entry| entry.name.clone())
            });
        let function_identity = operation
            .map(|entry| entry.function_identity.0.clone())
            .or_else(|| test.map(|entry| entry.function_identity.0.clone()))
            .or_else(|| {
                target_entry.and_then(|entry| entry.identity.as_ref().map(|id| id.0.clone()))
            });
        let test_declaration_identity = test.map(|entry| entry.declaration_identity.0.clone());
        let test_case_identity = test.map(|entry| entry.test_case_identity.0.clone());
        let source_span = operation
            .and_then(|entry| entry.source_span)
            .or_else(|| test.map(|entry| entry.source_span))
            .or_else(|| target_entry.map(|entry| entry.full_span))
            .or_else(|| {
                operation.and_then(|_entry| {
                    function_identity.as_ref().and_then(|identity| {
                        execution_source_map.and_then(|source_map| {
                            source_map
                                .functions
                                .iter()
                                .find(|function| function.identity.as_str() == identity)
                                .map(|function| function.declaration_span)
                        })
                    })
                })
            })
            .expect("subject or test supplied a span");
        let runtime_operation_source_span = operation
            .and_then(|entry| entry.source_span)
            .map(|span| snapshot.positions.range_of(text, span));

        let binding = DebugSourceBinding {
            schema_version: DEBUG_SOURCE_BINDING_SCHEMA_VERSION.to_owned(),
            source_identity: snapshot.source_identity.clone(),
            uri: uri.to_owned(),
            language_profile: snapshot.language_profile.clone(),
            module_name: module.clone(),
            module_identity: mncs_model::module_id(&module).0,
            function_name,
            function_identity,
            test_declaration_identity,
            test_case_identity,
            source_span: snapshot.positions.range_of(text, source_span),
            symbol_resolution: DebugBindingResolution::Exact,
            failure_location: None,
            runtime_operation_identity: operation.map(|entry| entry.identity.0.clone()),
            runtime_operation_source_span,
            runtime_operation_resolution: DebugCapabilityState {
                status: if operation.is_some() {
                    if runtime_operation_source_span.is_some() {
                        DebugCapabilityStatus::Supported
                    } else {
                        DebugCapabilityStatus::PartiallySupported
                    }
                } else {
                    DebugCapabilityStatus::Unsupported
                },
                reason: if operation.is_some() {
                    if runtime_operation_source_span.is_some() {
                        "operation identity and exact source span resolved from mncs.execution-source-map/1".to_owned()
                    } else {
                        "operation identity resolved, but the compiler marked this operation synthetic without a source span".to_owned()
                    }
                } else {
                    "declaration query has no runtime operation identity".to_owned()
                },
            },
            failure_location_resolution: DebugCapabilityState {
                status: DebugCapabilityStatus::Unsupported,
                reason: "failure locations remain debugger/runtime evidence, not declaration-span guesses".to_owned(),
            },
            breakpoint_resolution: DebugCapabilityState {
                status: if operation.is_some() {
                    DebugCapabilityStatus::PartiallySupported
                } else {
                    DebugCapabilityStatus::Unsupported
                },
                reason: if operation.is_some() {
                    "source operation resolution is available, but live suspension/stop control is not implemented".to_owned()
                } else {
                    "live breakpoint resolution is not implemented; clients may use the exact source_span as a navigation anchor".to_owned()
                },
            },
        };
        Ok(DebugSourceBindingResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            binding: Some(binding),
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
            targets.extend(self.targets_for_reference(uri, snapshot, reference));
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

    /// Resolve a reference through the authoritative declaration span. Local
    /// references are already indexed in `snapshot`; imported references point
    /// at a declaration span from the imported source, so join them against
    /// resident document indexes without guessing from text alone.
    pub(crate) fn targets_for_reference(
        &self,
        uri: &str,
        snapshot: &DocumentAnalysis,
        reference: &indexes::ReferenceEntry,
    ) -> Vec<SymbolSummary> {
        let local = reference
            .target
            .map(|target| summarize(uri, snapshot, target));
        let name = local
            .as_ref()
            .map(|summary| summary.name.as_str())
            .unwrap_or(reference.target_name.as_str());
        let identity = local
            .as_ref()
            .and_then(|summary| summary.identity.as_deref());
        let mut matches =
            self.symbols_matching(reference.declaration_span, reference.kind, name, identity);
        if matches.is_empty() {
            if let Some(local) = local {
                matches.push(local);
            }
        }
        matches
    }

    pub(crate) fn symbols_matching(
        &self,
        declaration: mncs_syntax::SourceSpan,
        kind: SymbolKind,
        name: &str,
        identity: Option<&str>,
    ) -> Vec<SymbolSummary> {
        let _ = self.ensure_workspace_entries();
        let key = crate::workspace_index::DeclarationKey {
            start: declaration.start,
            end: declaration.end,
            line: declaration.line,
            column: declaration.column,
            kind,
            name: name.to_owned(),
        };
        let Ok(index) = self.workspace_index.read() else {
            return Vec::new();
        };
        let mut matches: Vec<SymbolSummary> = index
            .lookup_declarations(&key)
            .into_iter()
            .filter_map(|(uri, symbol)| {
                let summary = index.entry(&uri)?.symbols.get(symbol)?.clone();
                if identity.is_some_and(|wanted| summary.identity.as_deref() != Some(wanted)) {
                    return None;
                }
                Some(summary)
            })
            .collect();
        matches.sort_by(|left, right| left.uri.cmp(&right.uri));
        matches.dedup();
        matches
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

        let reference = snapshot.symbols.references_at(byte).next();
        let declaration = snapshot.symbols.declaration_at(byte);
        let (declaration_span, kind, target_name, target_summary, target_identity) =
            if let Some(reference) = reference {
                let target_summary = reference
                    .target
                    .map(|target| summarize(uri, &snapshot, target));
                let target_name = target_summary
                    .as_ref()
                    .map(|summary| summary.name.clone())
                    .unwrap_or_else(|| reference.target_name.clone());
                let target_identity = target_summary
                    .as_ref()
                    .and_then(|summary| summary.identity.as_deref().map(str::to_owned));
                (
                    reference.declaration_span,
                    reference.kind,
                    target_name,
                    target_summary,
                    target_identity,
                )
            } else if let Some(target) = declaration {
                let summary = summarize(uri, &snapshot, target);
                (
                    snapshot.symbols.symbols[target].name_span,
                    snapshot.symbols.symbols[target].kind,
                    summary.name.clone(),
                    Some(summary.clone()),
                    summary.identity.clone(),
                )
            } else {
                return Ok(ReferencesResponse {
                    status: ResponseStatus::Unresolved {
                        reason: "no resolved symbol at position".to_owned(),
                    },
                    snapshot: Some(snapshot_info(uri, &snapshot)),
                    hits: Vec::new(),
                });
            };
        let mut declaration_matches = self.symbols_matching(
            declaration_span,
            kind,
            &target_name,
            target_identity.as_deref(),
        );
        if declaration_matches.is_empty() {
            if let Some(target_summary) = target_summary.clone() {
                declaration_matches.push(target_summary);
            }
        }
        let mut hits = Vec::new();
        if include_declaration {
            for declaration in declaration_matches {
                let declaration_uri = declaration.uri.clone().unwrap_or_else(|| uri.to_owned());
                hits.push(ReferenceHit {
                    uri: declaration_uri,
                    is_declaration: true,
                    range: declaration.range,
                    name_range: declaration.name_range,
                    kind: declaration.kind,
                    name: declaration.name,
                    container: declaration.container,
                });
            }
        }
        let name = target_name;
        let _ = self.ensure_workspace_entries();
        let key = crate::workspace_index::OccurrenceKey {
            decl_start: declaration_span.start,
            decl_end: declaration_span.end,
            decl_line: declaration_span.line,
            decl_column: declaration_span.column,
            kind,
            name: Some(name.clone()),
        };
        let occurrences = self
            .workspace_index
            .read()
            .ok()
            .map(|index| index.lookup_occurrences(&key))
            .unwrap_or_default();
        for occurrence in occurrences {
            hits.push(ReferenceHit {
                uri: occurrence.uri,
                is_declaration: false,
                range: occurrence.range,
                name_range: occurrence.range,
                kind,
                name: name.clone(),
                container: None,
            });
        }
        hits.sort_by(|left, right| {
            left.uri
                .cmp(&right.uri)
                .then(left.range.start_byte.cmp(&right.range.start_byte))
        });
        Ok(ReferencesResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            hits,
        })
    }

    pub(crate) fn primary_symbol_index(
        &self,
        snapshot: &DocumentAnalysis,
        byte: usize,
    ) -> Option<usize> {
        snapshot
            .symbols
            .references_at(byte)
            .next()
            .and_then(|reference| reference.target)
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
        let _ = self.ensure_workspace_entries();
        let mut hits = Vec::new();
        if let Ok(index) = self.workspace_index.read() {
            for (uri, entry) in index.entries() {
                for symbol in entry.symbols_matching(&needle) {
                    hits.push(WorkspaceSymbolHit {
                        uri: (*uri).clone(),
                        summary: entry.symbols[symbol].clone(),
                    });
                }
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
        let resolved = if let Some(target) = self.primary_symbol_index(&snapshot, byte) {
            Some((snapshot.clone(), target, summarize(uri, &snapshot, target)))
        } else if let Some(reference) = snapshot.symbols.references_at(byte).next() {
            self.targets_for_reference(uri, &snapshot, reference)
                .into_iter()
                .next()
                .and_then(|summary| {
                    let target_uri = summary.uri.clone()?;
                    let target_snapshot = self.snapshot(&target_uri).ok()?;
                    let target_index =
                        target_snapshot.symbols.symbols.iter().position(|entry| {
                            entry.name == summary.name
                                && entry.name_span.start == summary.name_range.start_byte
                                && entry.kind == summary.kind
                        })?;
                    Some((target_snapshot, target_index, summary))
                })
        } else {
            None
        };
        let Some((render_snapshot, target, summary)) = resolved else {
            return Ok(HoverResponse {
                status: ResponseStatus::Unresolved {
                    reason: "no resolvable subject under the cursor".to_owned(),
                },
                snapshot: Some(snapshot_info(uri, &snapshot)),
                subject: None,
                markdown: None,
            });
        };
        let markdown = render_hover_markdown(&render_snapshot, target);
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

    /// Modules that transitively import `uri`'s module, breadth-first over
    /// the resident reverse-dependency index. The subject itself is excluded;
    /// direct importers are depth 1.
    pub fn transitive_dependents(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        let max_depth = max_depth.clamp(1, MAX_TRANSITIVE_DEPTH);
        // Validates that the subject is a readable document.
        let _ = self.snapshot(uri)?;
        let _ = self.ensure_workspace_entries();
        let index = self.workspace_index.read().map_err(poisoned)?;
        let mut visited: BTreeSet<String> = BTreeSet::from([uri.to_owned()]);
        let mut frontier = vec![uri.to_owned()];
        let mut nodes = Vec::new();
        let mut complete = true;
        for depth in 1..=max_depth {
            let mut next = Vec::new();
            for current in &frontier {
                for importer in index.importers_of(current) {
                    // Skip edges orphaned by a module move: the recorded
                    // module must still resolve to the walked URI.
                    let edge = index.entry(&importer).and_then(|entry| {
                        entry.deps.iter().find(|edge| {
                            &edge.uri == current
                                && self.store.module_uri(&edge.module).as_deref() == Some(current)
                        })
                    });
                    let Some(edge) = edge else {
                        continue;
                    };
                    if !visited.insert(importer.clone()) {
                        continue;
                    }
                    nodes.push(TransitiveDepNode {
                        uri: importer.clone(),
                        module: index
                            .entry(&importer)
                            .map(|entry| entry.module.clone())
                            .unwrap_or_default(),
                        depth,
                        via_module: Some(edge.module.clone()),
                    });
                    if nodes.len() >= MAX_TRANSITIVE_NODES {
                        complete = false;
                        break;
                    }
                    next.push(importer);
                }
                if !complete {
                    break;
                }
            }
            if !complete || next.is_empty() {
                break;
            }
            frontier = next;
        }
        nodes.sort_by(|left, right| (left.depth, &left.uri).cmp(&(right.depth, &right.uri)));
        Ok(TransitiveDepsResponse {
            status: ResponseStatus::Answered,
            generation: self.store.generation(),
            nodes,
            complete,
        })
    }

    /// Modules that `uri`'s module transitively imports, breadth-first over
    /// the resident dependency edges. The subject itself is excluded even
    /// when an import cycle leads back to it; direct dependencies are
    /// depth 1.
    pub fn transitive_dependencies(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        let max_depth = max_depth.clamp(1, MAX_TRANSITIVE_DEPTH);
        let _ = self.snapshot(uri)?;
        let _ = self.ensure_workspace_entries();
        let index = self.workspace_index.read().map_err(poisoned)?;
        let mut visited: BTreeSet<String> = BTreeSet::from([uri.to_owned()]);
        let mut frontier = vec![uri.to_owned()];
        let mut nodes = Vec::new();
        let mut complete = true;
        for depth in 1..=max_depth {
            let mut next = Vec::new();
            for current in &frontier {
                let Some(entry) = index.entry(current) else {
                    continue;
                };
                for edge in &entry.deps {
                    // Follow live resolution so a moved module is reported
                    // at its current owner. External files have no live
                    // resolution and keep their recorded URI.
                    let target = if self.store.knows(&edge.uri) {
                        self.store
                            .module_uri(&edge.module)
                            .unwrap_or_else(|| edge.uri.clone())
                    } else {
                        edge.uri.clone()
                    };
                    if !visited.insert(target.clone()) {
                        continue;
                    }
                    nodes.push(TransitiveDepNode {
                        uri: target.clone(),
                        module: index
                            .entry(&target)
                            .map(|entry| entry.module.clone())
                            .filter(|module| !module.is_empty())
                            .unwrap_or_else(|| edge.module.clone()),
                        depth,
                        via_module: Some(edge.module.clone()),
                    });
                    if nodes.len() >= MAX_TRANSITIVE_NODES {
                        complete = false;
                        break;
                    }
                    next.push(target);
                }
                if !complete {
                    break;
                }
            }
            if !complete || next.is_empty() {
                break;
            }
            frontier = next;
        }
        nodes.sort_by(|left, right| (left.depth, &left.uri).cmp(&(right.depth, &right.uri)));
        Ok(TransitiveDepsResponse {
            status: ResponseStatus::Answered,
            generation: self.store.generation(),
            nodes,
            complete,
        })
    }

    /// Functions that transitively call `identity`, breadth-first over the
    /// resident reference index. Call sites come from the compiler's
    /// authoritative name resolutions (the same join as `incoming_calls`),
    /// so this crosses module boundaries without text search. Direct
    /// callers are depth 1.
    pub fn transitive_callers(
        &self,
        identity: &str,
        max_depth: usize,
    ) -> Result<TransitiveCallersResponse, ServiceError> {
        let max_depth = max_depth.clamp(1, MAX_TRANSITIVE_DEPTH);
        let _ = self.ensure_workspace_entries();
        let index = self.workspace_index.read().map_err(poisoned)?;
        let Some((root_uri, root_idx)) = index.lookup_identity(identity) else {
            return Ok(TransitiveCallersResponse {
                status: ResponseStatus::Unresolved {
                    reason: format!("identity {identity} names no workspace symbol"),
                },
                subject_identity: identity.to_owned(),
                nodes: Vec::new(),
                complete: true,
            });
        };
        let root_kind = index
            .entry(&root_uri)
            .and_then(|entry| entry.symbols.get(root_idx))
            .map(|summary| summary.kind);
        if root_kind != Some(SymbolKind::Function) {
            return Ok(TransitiveCallersResponse {
                status: ResponseStatus::Unresolved {
                    reason: "only functions participate in the call hierarchy".to_owned(),
                },
                subject_identity: identity.to_owned(),
                nodes: Vec::new(),
                complete: true,
            });
        }
        // Expand by (uri, symbol) so callers without a recorded identity
        // still contribute their own callers to deeper levels.
        let root = (root_uri, root_idx);
        let mut visited: BTreeSet<(String, usize)> = BTreeSet::from([root.clone()]);
        let mut expand = vec![root];
        let mut nodes = Vec::new();
        let mut complete = true;
        for depth in 1..=max_depth {
            let mut next = Vec::new();
            for (uri, idx) in &expand {
                let Some(entry) = index.entry(uri) else {
                    continue;
                };
                let Some(span) = entry.spans.get(*idx).copied() else {
                    continue;
                };
                for occurrence in index.occurrences_for_declaration(span, SymbolKind::Function) {
                    let Some(caller) = occurrence.enclosing_function else {
                        continue;
                    };
                    let key = (occurrence.uri.clone(), caller);
                    if !visited.insert(key.clone()) {
                        continue;
                    }
                    let summary = index
                        .entry(&occurrence.uri)
                        .and_then(|entry| entry.symbols.get(caller));
                    nodes.push(TransitiveCallerNode {
                        identity: summary.and_then(|summary| summary.identity.clone()),
                        uri: occurrence.uri.clone(),
                        name: summary
                            .map(|summary| summary.name.clone())
                            .unwrap_or_default(),
                        depth,
                    });
                    if nodes.len() >= MAX_TRANSITIVE_NODES {
                        complete = false;
                        break;
                    }
                    next.push(key);
                }
                if !complete {
                    break;
                }
            }
            if !complete || next.is_empty() {
                break;
            }
            expand = next;
        }
        nodes.sort_by(|left, right| {
            (left.depth, &left.uri, &left.name).cmp(&(right.depth, &right.uri, &right.name))
        });
        Ok(TransitiveCallersResponse {
            status: ResponseStatus::Answered,
            subject_identity: identity.to_owned(),
            nodes,
            complete,
        })
    }

    pub(crate) fn graph_query(
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

    /// Execute the bounded MNCS-native status kernel against the exact
    /// obligation projection produced by the authoritative frontend.
    ///
    /// This is an explicit experimental path: the ordinary `obligations`
    /// query remains the Rust control implementation, while this method
    /// retains both sides of the differential comparison and fails closed on
    /// missing library/backend support, invalid values, or disagreement.
    pub fn native_obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<NativeObligationsResponse, ServiceError> {
        let reference = self.obligations_response(uri, subject_identity)?;
        let snapshot = reference.snapshot.clone();
        if reference.status != ResponseStatus::Answered {
            return Ok(NativeObligationsResponse {
                status: reference.status,
                snapshot,
                obligations: reference.obligations,
                reference_counts: reference.counts,
                counts: reference.counts,
                native: None,
                unresolved: vec![
                    "native status execution was not attempted because the authoritative program is unavailable"
                        .to_owned(),
                ],
            });
        }

        let statuses = reference
            .obligations
            .iter()
            .map(|obligation| obligation.status.as_str())
            .collect::<Vec<_>>();
        let native = crate::native_query::execute_status_summary(
            &self.native_kernel,
            &self.store,
            &statuses,
        );
        let reference_counts = reference.counts;
        match native {
            Ok(native) => {
                let native_counts = StatusCounts {
                    pass: native.pass_count,
                    fail: native.fail_count,
                    unknown: native.unknown_count,
                };
                let expected_status = dominant_status(&reference_counts);
                let mut unresolved = Vec::new();
                if native_counts != reference_counts {
                    unresolved.push(format!(
                        "MNCS-native status counts disagree with the Rust control result: reference={reference_counts:?}, native={native_counts:?}"
                    ));
                }
                if native.observed_count != statuses.len() {
                    unresolved.push(format!(
                        "MNCS-native status kernel observed {} of {} projected obligations",
                        native.observed_count,
                        statuses.len()
                    ));
                }
                if !native.valid {
                    unresolved
                        .push("MNCS-native status kernel rejected its bounded envelope".to_owned());
                }
                if native.dominant_status != expected_status {
                    unresolved.push(format!(
                        "MNCS-native dominant status {:?} disagrees with the Rust control result {:?}",
                        native.dominant_status, expected_status
                    ));
                }
                let status = if unresolved.is_empty() {
                    ResponseStatus::Answered
                } else {
                    ResponseStatus::Unsupported {
                        reason: "MNCS-native status query did not agree with the authoritative Rust control result"
                            .to_owned(),
                    }
                };
                Ok(NativeObligationsResponse {
                    status,
                    snapshot,
                    obligations: reference.obligations,
                    reference_counts,
                    counts: native_counts,
                    native: Some(native),
                    unresolved,
                })
            }
            Err(reason) => Ok(NativeObligationsResponse {
                status: ResponseStatus::Unsupported {
                    reason: format!("MNCS-native status query unavailable: {reason}"),
                },
                snapshot,
                obligations: reference.obligations,
                reference_counts,
                counts: reference_counts,
                native: None,
                unresolved: vec![reason],
            }),
        }
    }

    /// Execute the bounded MNCS-native kind filter against the document's
    /// symbol-index projection.
    ///
    /// The second experimental native kernel (after obligation statuses):
    /// the Rust control projects the first [`MAX_FILTER_TAGS`] indexed
    /// symbols to stable kind tags, the MNCS kernel counts the wanted tag
    /// through the authoritative generic `mncs.core.sequences.v1::count`,
    /// and the response fails closed on any disagreement. This is the
    /// roadmap's bounded-symbol-filtering pressure point made executable.
    pub fn native_kind_count(
        &self,
        uri: &str,
        wanted: SymbolKind,
    ) -> Result<NativeKindCountResponse, ServiceError> {
        use crate::native_filter::{execute_count_matching, symbol_kind_tag, MAX_FILTER_TAGS};

        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let tags: Vec<i64> = snapshot
            .symbols
            .symbols
            .iter()
            .take(MAX_FILTER_TAGS)
            .map(|entry| symbol_kind_tag(entry.kind))
            .collect();
        let wanted_tag = symbol_kind_tag(wanted);
        let reference_count = tags.iter().filter(|tag| **tag == wanted_tag).count();
        match execute_count_matching(&self.filter_kernel, &self.store, &tags, wanted_tag) {
            Ok(native) => {
                let mut unresolved = Vec::new();
                if native.native_count != reference_count
                    || native.reference_count != reference_count
                {
                    unresolved.push(format!(
                        "MNCS-native kind count disagrees with the Rust control result: reference={reference_count}, native={}",
                        native.native_count
                    ));
                }
                if !native.valid {
                    unresolved
                        .push("MNCS-native filter kernel rejected its bounded envelope".to_owned());
                }
                let status = if unresolved.is_empty() {
                    ResponseStatus::Answered
                } else {
                    ResponseStatus::Unsupported {
                        reason: "MNCS-native kind filter did not agree with the authoritative Rust control result"
                            .to_owned(),
                    }
                };
                Ok(NativeKindCountResponse {
                    status,
                    snapshot: Some(info()),
                    wanted_kind: wanted,
                    wanted_tag,
                    input_tags: tags,
                    reference_count,
                    native_count: native.native_count,
                    native: Some(native),
                    unresolved,
                })
            }
            Err(reason) => Ok(NativeKindCountResponse {
                status: ResponseStatus::Unsupported {
                    reason: format!("MNCS-native kind filter unavailable: {reason}"),
                },
                snapshot: Some(info()),
                wanted_kind: wanted,
                wanted_tag,
                input_tags: tags,
                reference_count,
                native_count: reference_count,
                native: None,
                unresolved: vec![reason],
            }),
        }
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

/// Result of the second MNCS-native service query (bounded symbol-kind
/// filtering). `reference_count` is the Rust control result over the
/// projected `input_tags`; `native_count` is the independently executed
/// MNCS result. `Answered` only on agreement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeKindCountResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    pub wanted_kind: SymbolKind,
    pub wanted_tag: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_tags: Vec<i64>,
    pub reference_count: usize,
    pub native_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<crate::native_filter::NativeFilterSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

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

/// Drop cached snapshots and index entries for `changed` and all of its
/// transitive importers. Invoked from the store's change callback on the
/// mutating thread with no store guards held; locks are taken sequentially
/// (never nested) so this cannot deadlock against query paths.
fn invalidate_transitive_importers(
    analyses: &RwLock<BTreeMap<String, AnalysisSlot>>,
    workspace_index: &RwLock<crate::workspace_index::WorkspaceIndex>,
    changed: &str,
) {
    let victims: Vec<String> = {
        let Ok(index) = workspace_index.read() else {
            return;
        };
        let mut visited = BTreeSet::from([changed.to_owned()]);
        let mut frontier = vec![changed.to_owned()];
        let mut victims = vec![changed.to_owned()];
        while let Some(current) = frontier.pop() {
            for importer in index.importers_of(&current) {
                if visited.insert(importer.clone()) {
                    frontier.push(importer.clone());
                    victims.push(importer);
                }
            }
        }
        victims
    };
    if victims.is_empty() {
        return;
    }
    if let Ok(mut writable) = analyses.write() {
        for uri in &victims {
            writable.remove(uri);
        }
    }
    if let Ok(mut writable) = workspace_index.write() {
        for uri in &victims {
            writable.remove(uri);
        }
    }
}

/// Public projection helper used by the shared renderer.
pub fn summarize_public(uri: &str, snapshot: &DocumentAnalysis, index: usize) -> SymbolSummary {
    summarize(uri, snapshot, index)
}

pub(crate) fn snapshot_info(uri: &str, snapshot: &DocumentAnalysis) -> SnapshotInfo {
    SnapshotInfo {
        uri: uri.to_owned(),
        source_identity: snapshot.source_identity.clone(),
        generation: snapshot.generation,
        language_profile: snapshot.language_profile.clone(),
        current: true,
    }
}

fn module_name(snapshot: &DocumentAnalysis) -> String {
    snapshot
        .front_end
        .ast
        .as_ref()
        .map(|ast| ast.module.text.clone())
        .or_else(|| {
            snapshot
                .front_end
                .program
                .as_ref()
                .map(|program| program.module.clone())
        })
        .unwrap_or_default()
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

fn dominant_status(counts: &StatusCounts) -> String {
    if counts.fail > 0 {
        "fail"
    } else if counts.unknown > 0 {
        "unknown"
    } else if counts.pass > 0 {
        "pass"
    } else {
        // The MNCS status lattice starts an empty bounded envelope at UNKNOWN;
        // keep the Rust control interpretation aligned with that behavior.
        "unknown"
    }
    .to_owned()
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

pub(crate) fn render_diagnostics(
    snapshot: &DocumentAnalysis,
    store: &crate::document::DocumentStore,
) -> Vec<DiagnosticItem> {
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
            related: render_related(snapshot, store, diagnostic),
        })
        .collect()
}

/// Project causal inner diagnostics to their owning locations.
///
/// Import-boundary wraps (`MNE172`) carry leaf diagnostics whose spans are
/// relative to the failing dependency's source, *not* the importing file, so
/// projecting them here would point at wrong text. The outer span covers the
/// `use` module name, which resolves through the same store resolver used
/// for analysis; when the dependency is resident its own snapshot projects
/// the exact range, otherwise the entry keeps the owning URI with no range
/// rather than a guessed one.
fn render_related(
    snapshot: &DocumentAnalysis,
    store: &crate::document::DocumentStore,
    outer: &mncs_syntax::SourceDiagnostic,
) -> Vec<DiagnosticRelated> {
    if outer.related.is_empty() {
        return Vec::new();
    }
    let text = snapshot.text();
    let dependency_uri = text
        .get(outer.span.start..outer.span.end)
        .and_then(parse_use_target)
        .and_then(|module| {
            use mncs_compiler::ModuleResolver;
            crate::modules::StoreResolver::new(store)
                .resolve(&module)
                .and_then(|envelope| envelope.origin.locator)
        });
    outer
        .related
        .iter()
        .map(|leaf| {
            let (uri, range) = match dependency_uri.clone() {
                Some(uri) => (Some(uri.clone()), project_leaf(store, &uri, leaf.span)),
                None => (None, None),
            };
            DiagnosticRelated {
                code: leaf.code.clone(),
                message: leaf.message.clone(),
                uri,
                range,
            }
        })
        .collect()
}

/// Best-effort range projection of a leaf span into a resident dependency
/// snapshot. `None` when the dependency is not resident: a file-level
/// reference beats a wrong range.
fn project_leaf(
    store: &crate::document::DocumentStore,
    uri: &str,
    span: mncs_syntax::SourceSpan,
) -> Option<Range> {
    let text = store.content(uri).ok()?;
    // Projecting requires that exact content; the snapshot layer owns the
    // authoritative mapping, so rebuild it from the resident text. This is
    // the same translation the owning document's own diagnostics use.
    let map = crate::coords::PositionMap::new(&text);
    if span.start > text.len() || span.end > text.len() {
        return None;
    }
    Some(map.range_of(&text, span))
}

/// Read a `use` target from the outer diagnostic span: either the bare
/// module name (`MNE172` covers the name token) or a full `use x.y;` line.
fn parse_use_target(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("use").unwrap_or(trimmed).trim();
    let rest = rest.strip_suffix(';').unwrap_or(rest).trim();
    // Module names are lowercase/alphanumeric/dot/underscore segments.
    if rest.is_empty()
        || !rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        || rest.starts_with('.')
        || rest.ends_with('.')
        || rest.contains("..")
    {
        return None;
    }
    Some(rest.to_owned())
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
