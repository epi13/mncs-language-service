//! Resident semantic core of the MNCS Language Service.
//!
//! This crate owns everything the service is allowed to own and nothing it is
//! not: workspace/document state, identity-bound analysis snapshots produced
//! by the authoritative `mncs-language` frontend, symbol/reference indexes
//! derived from those artifacts, source-coordinate translation, and a
//! protocol-neutral query layer consumed by the LSP and MCP adapters.
//!
//! It deliberately contains no grammar, typing, validation, identity, graph,
//! obligation, or evidence logic of its own. Every semantic fact originates in
//! `mncs-syntax`, `mncs-compiler`, or `mncs-model`.

mod actions;
mod analysis;
mod candidate;
mod coords;
mod document;
pub mod edits;
mod error;
pub mod format;
mod indexes;
pub mod intel;
mod modules;
mod native_filter;
mod native_query;
mod queries;
mod rename;
mod render;

pub use actions::{CodeAction, CodeActionsResponse};
pub use analysis::DocumentAnalysis;
pub use candidate::{
    CandidateAnalysisResponse, CandidateObligation, ChangedIdentity, DiagnosticsDelta,
    ObligationDelta, ObligationStatusChange, SemanticDelta, StaleEvidenceItem,
};
pub use coords::{PositionInfo, PositionMap, RangeInfo};
pub use document::{DocumentStore, MAX_DISCOVERED_DOCUMENTS, MAX_DOCUMENT_BYTES};
pub use edits::{TextChange, TextRange};
pub use error::ServiceError;
pub use format::{format_text, FormattingResponse, RangeFormattingResponse};
pub use indexes::{ReferenceEntry, SymbolEntry, SymbolIndex, SymbolKind};
pub use intel::{
    CallHierarchyCallsResponse, CallHierarchyEdge, CallHierarchyItem, InlayHintItem, InlayHintKind,
    InlayHintsResponse, PrepareCallHierarchyResponse, SelectionChain, SelectionRangesResponse,
    SignatureHelpResponse, SignatureParameter,
};
pub use native_filter::{symbol_kind_tag, NativeFilterSummary, FILTER_PADDING_TAG};
pub use native_query::NativeStatusSummary;
pub use queries::{
    CompletionCandidate, CompletionClass, ContextExcerpt, ContextPacketResponse,
    DefinitionResponse, DescribeResponse, DiagnosticItem, DiagnosticRelated, DiagnosticsResponse,
    DocumentStatusEntry, DocumentSymbolNode, DocumentSymbolsResponse, EffectInfo, FoldRange,
    FoldingRangesResponse, GraphEdgeTarget, GraphResponse, HighlightsResponse, HoverResponse,
    LanguageService, MemberInfo, NativeKindCountResponse, NativeObligationsResponse,
    ObligationInfo, ObligationsResponse, Occurrence, OccurrenceRole, PositionQueryResponse,
    ReferenceHit, ReferencesResponse, ResponseStatus, SemanticTokensResponse, SnapshotInfo,
    StatusCounts, SubjectDescription, SymbolSummary, TokenAnnotation, TokenClass,
    WorkspaceStatusResponse, WorkspaceSymbolHit, WorkspaceSymbolsResponse,
};
pub use rename::{FileEdit, RenameResponse, SingleEdit};
