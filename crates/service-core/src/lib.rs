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

mod analysis;
mod coords;
mod document;
mod error;
mod indexes;
mod queries;
mod render;

pub use analysis::DocumentAnalysis;
pub use coords::{PositionInfo, PositionMap, RangeInfo};
pub use document::{DocumentStore, MAX_DISCOVERED_DOCUMENTS, MAX_DOCUMENT_BYTES};
pub use error::ServiceError;
pub use indexes::{ReferenceEntry, SymbolEntry, SymbolIndex, SymbolKind};
pub use queries::{
    CompletionCandidate, CompletionClass, ContextExcerpt, ContextPacketResponse,
    DefinitionResponse, DescribeResponse, DiagnosticItem, DiagnosticsResponse, DocumentStatusEntry,
    DocumentSymbolNode, DocumentSymbolsResponse, EffectInfo, FoldRange, FoldingRangesResponse,
    GraphEdgeTarget, GraphResponse, HighlightsResponse, HoverResponse, LanguageService, MemberInfo,
    ObligationInfo, ObligationsResponse, Occurrence, OccurrenceRole, PositionQueryResponse,
    ReferenceHit, ReferencesResponse, ResponseStatus, SemanticTokensResponse, SnapshotInfo,
    StatusCounts, SubjectDescription, SymbolSummary, TokenAnnotation, TokenClass,
    WorkspaceStatusResponse, WorkspaceSymbolHit, WorkspaceSymbolsResponse,
};
