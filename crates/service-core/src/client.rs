//! The canonical protocol-neutral client boundary for the resident service.
//!
//! LSP, MCP, and machine-native clients all use this one trait.  The local
//! implementation is the resident core itself; the Unix-socket implementation
//! is a thin request/response transport to that same core.  No adapter gets a
//! private semantic implementation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    CallHierarchyCallsResponse, CandidateAnalysisResponse, CodeActionsResponse, CompletionResponse,
    ContextPacketResponse, DebugSourceBindingResponse, DefinitionResponse, DescribeResponse,
    DiagnosticsResponse, DocumentSymbolsResponse, FamilyAgentContextResponse,
    FoldingRangesResponse, FormattingResponse, GraphResponse, HighlightsResponse, HoverResponse,
    InlayHintsResponse, LanguageCapabilitiesResponse, LanguageService, NativeKindCountResponse,
    NativeObligationsResponse, PositionQueryResponse, PrepareCallHierarchyResponse,
    RangeFormattingResponse, ReferencesResponse, RenameResponse, SelectionRangesResponse,
    SemanticCapsuleResponse, SemanticImpactResponse, SemanticTokensResponse, ServiceError,
    ServiceStatsSnapshot, ServiceStatusResponse, SignatureHelpResponse, TextChange,
    TransitiveCallersResponse, TransitiveDepsResponse, WorkspaceEventCursor,
    WorkspaceStatusResponse, WorkspaceSymbolsResponse,
};

/// Shared service surface consumed by local protocol adapters and remote
/// machine clients.  Methods preserve the core's explicit error/UNKNOWN
/// distinctions instead of returning empty success values.
pub trait LanguageServiceClient: Send + Sync {
    fn workspace_root(&self) -> Option<PathBuf>;
    fn configure_root(&self, root: Option<PathBuf>) -> Result<Vec<String>, ServiceError>;
    fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError>;
    fn did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<TextChange>,
    ) -> Result<u64, ServiceError>;
    fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError>;
    fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError>;
    fn content(&self, uri: &str) -> Result<String, ServiceError>;
    fn buffer_version(&self, uri: &str) -> Result<Option<i32>, ServiceError>;
    fn refresh_workspace(&self) -> Result<Vec<u64>, ServiceError>;
    fn workspace_status(&self) -> Result<WorkspaceStatusResponse, ServiceError>;
    fn service_status(&self) -> Result<ServiceStatusResponse, ServiceError>;
    fn service_stats(&self) -> Result<ServiceStatsSnapshot, ServiceError>;
    fn semantic_capsule(
        &self,
        known_stream_identity: Option<&str>,
        known_cursor: u64,
    ) -> Result<SemanticCapsuleResponse, ServiceError>;
    fn poll_events(&self, after_cursor: u64, max_events: usize) -> WorkspaceEventCursor;

    fn language_capabilities(
        &self,
        topic: Option<&str>,
        symbol: Option<&str>,
        profile: Option<&str>,
        delta_from: Option<&str>,
        known_identity: Option<&str>,
        max_items: usize,
    ) -> Result<LanguageCapabilitiesResponse, ServiceError>;
    fn family_agent_context(
        &self,
        repository: Option<&str>,
        topic: Option<&str>,
        symbol: Option<&str>,
        known_language_identity: Option<&str>,
        known_architecture_identity: Option<&str>,
        max_items: usize,
    ) -> Result<FamilyAgentContextResponse, ServiceError>;
    fn document_diagnostics(&self, uri: &str) -> Result<DiagnosticsResponse, ServiceError>;
    fn subjects_at(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PositionQueryResponse, ServiceError>;
    fn debug_source_binding(
        &self,
        uri: &str,
        identity: Option<&str>,
        line: Option<u32>,
        character: Option<u32>,
    ) -> Result<DebugSourceBindingResponse, ServiceError>;
    fn describe_identity(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<DescribeResponse, ServiceError>;
    fn describe_position(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DescribeResponse, ServiceError>;
    fn definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError>;
    fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResponse, ServiceError>;
    fn document_symbols(&self, uri: &str) -> Result<DocumentSymbolsResponse, ServiceError>;
    fn workspace_symbols(&self, query: &str) -> Result<WorkspaceSymbolsResponse, ServiceError>;
    fn dependencies(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError>;
    fn dependents(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError>;
    fn transitive_dependents(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError>;
    fn transitive_dependencies(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError>;
    fn transitive_callers(
        &self,
        identity: &str,
        max_depth: usize,
    ) -> Result<TransitiveCallersResponse, ServiceError>;
    fn semantic_impact(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<SemanticImpactResponse, ServiceError>;
    fn obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<crate::queries::ObligationsForUri, ServiceError>;
    fn native_obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<NativeObligationsResponse, ServiceError>;
    fn native_kind_count(
        &self,
        uri: &str,
        wanted: crate::indexes::SymbolKind,
    ) -> Result<NativeKindCountResponse, ServiceError>;
    fn context_packet(
        &self,
        uri: &str,
        identity: &str,
        max_excerpts: usize,
    ) -> Result<ContextPacketResponse, ServiceError>;
    fn analyze_candidate(
        &self,
        uri: &str,
        candidate_text: &str,
    ) -> Result<CandidateAnalysisResponse, ServiceError>;
    fn hover(&self, uri: &str, line: u32, character: u32) -> Result<HoverResponse, ServiceError>;
    fn highlights(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<HighlightsResponse, ServiceError>;
    fn signature_help(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<SignatureHelpResponse, ServiceError>;
    fn declaration(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError>;
    fn type_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError>;
    fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PrepareCallHierarchyResponse, ServiceError>;
    fn incoming_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError>;
    fn outgoing_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError>;

    fn references_for_lsp(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResponse, ServiceError> {
        self.references(uri, line, character, include_declaration)
    }
    fn semantic_tokens(&self, uri: &str) -> Result<SemanticTokensResponse, ServiceError>;
    fn folding_ranges(&self, uri: &str) -> Result<FoldingRangesResponse, ServiceError>;
    fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<RenameResponse, ServiceError>;
    fn formatting(&self, uri: &str) -> Result<FormattingResponse, ServiceError>;
    fn range_formatting(
        &self,
        uri: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<RangeFormattingResponse, ServiceError>;
    fn selection_ranges(
        &self,
        uri: &str,
        positions: &[(u32, u32)],
    ) -> Result<SelectionRangesResponse, ServiceError>;
    fn inlay_hints(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<InlayHintsResponse, ServiceError>;
    fn code_actions(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<CodeActionsResponse, ServiceError>;
    fn completion(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CompletionResponse, ServiceError>;
}

impl LanguageServiceClient for LanguageService {
    fn workspace_root(&self) -> Option<PathBuf> {
        LanguageService::workspace_root(self)
    }
    fn configure_root(&self, root: Option<PathBuf>) -> Result<Vec<String>, ServiceError> {
        LanguageService::configure_root(self, root)
    }
    fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        LanguageService::did_open(self, uri, version, text)
    }
    fn did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<TextChange>,
    ) -> Result<u64, ServiceError> {
        LanguageService::did_change_incremental(self, uri, version, changes)
    }
    fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError> {
        LanguageService::did_save(self, uri, text)
    }
    fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        LanguageService::did_close(self, uri)
    }
    fn content(&self, uri: &str) -> Result<String, ServiceError> {
        LanguageService::content(self, uri)
    }
    fn buffer_version(&self, uri: &str) -> Result<Option<i32>, ServiceError> {
        self.store().buffer_version(uri)
    }
    fn refresh_workspace(&self) -> Result<Vec<u64>, ServiceError> {
        LanguageService::refresh_workspace(self)
    }
    fn workspace_status(&self) -> Result<WorkspaceStatusResponse, ServiceError> {
        LanguageService::workspace_status(self)
    }
    fn service_status(&self) -> Result<ServiceStatusResponse, ServiceError> {
        LanguageService::service_status(self)
    }
    fn service_stats(&self) -> Result<ServiceStatsSnapshot, ServiceError> {
        Ok(LanguageService::service_stats(self))
    }
    fn semantic_capsule(
        &self,
        known_stream_identity: Option<&str>,
        known_cursor: u64,
    ) -> Result<SemanticCapsuleResponse, ServiceError> {
        LanguageService::semantic_capsule(self, known_stream_identity, known_cursor)
    }
    fn poll_events(&self, after_cursor: u64, max_events: usize) -> WorkspaceEventCursor {
        LanguageService::poll_events(self, after_cursor, max_events)
    }
    fn language_capabilities(
        &self,
        topic: Option<&str>,
        symbol: Option<&str>,
        profile: Option<&str>,
        delta_from: Option<&str>,
        known_identity: Option<&str>,
        max_items: usize,
    ) -> Result<LanguageCapabilitiesResponse, ServiceError> {
        LanguageService::language_capabilities(
            self,
            topic,
            symbol,
            profile,
            delta_from,
            known_identity,
            max_items,
        )
    }
    fn family_agent_context(
        &self,
        repository: Option<&str>,
        topic: Option<&str>,
        symbol: Option<&str>,
        known_language_identity: Option<&str>,
        known_architecture_identity: Option<&str>,
        max_items: usize,
    ) -> Result<FamilyAgentContextResponse, ServiceError> {
        LanguageService::family_agent_context(
            self,
            repository,
            topic,
            symbol,
            known_language_identity,
            known_architecture_identity,
            max_items,
        )
    }
    fn document_diagnostics(&self, uri: &str) -> Result<DiagnosticsResponse, ServiceError> {
        LanguageService::document_diagnostics(self, uri)
    }
    fn subjects_at(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PositionQueryResponse, ServiceError> {
        LanguageService::subjects_at(self, uri, line, character)
    }
    fn debug_source_binding(
        &self,
        uri: &str,
        identity: Option<&str>,
        line: Option<u32>,
        character: Option<u32>,
    ) -> Result<DebugSourceBindingResponse, ServiceError> {
        LanguageService::debug_source_binding(self, uri, identity, line, character)
    }
    fn describe_identity(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<DescribeResponse, ServiceError> {
        LanguageService::describe_identity(self, uri, identity)
    }
    fn describe_position(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DescribeResponse, ServiceError> {
        LanguageService::describe_position(self, uri, line, character)
    }
    fn definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        LanguageService::definition(self, uri, line, character)
    }
    fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResponse, ServiceError> {
        LanguageService::references(self, uri, line, character, include_declaration)
    }
    fn document_symbols(&self, uri: &str) -> Result<DocumentSymbolsResponse, ServiceError> {
        LanguageService::document_symbols(self, uri)
    }
    fn workspace_symbols(&self, query: &str) -> Result<WorkspaceSymbolsResponse, ServiceError> {
        Ok(LanguageService::workspace_symbols(self, query))
    }
    fn dependencies(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        LanguageService::dependencies(self, uri, identity)
    }
    fn dependents(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        LanguageService::dependents(self, uri, identity)
    }
    fn transitive_dependents(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        LanguageService::transitive_dependents(self, uri, max_depth)
    }
    fn transitive_dependencies(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        LanguageService::transitive_dependencies(self, uri, max_depth)
    }
    fn transitive_callers(
        &self,
        identity: &str,
        max_depth: usize,
    ) -> Result<TransitiveCallersResponse, ServiceError> {
        LanguageService::transitive_callers(self, identity, max_depth)
    }
    fn semantic_impact(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<SemanticImpactResponse, ServiceError> {
        LanguageService::semantic_impact(self, uri, identity)
    }
    fn obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<crate::queries::ObligationsForUri, ServiceError> {
        LanguageService::obligations(self, uri, subject_identity)
    }
    fn native_obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<NativeObligationsResponse, ServiceError> {
        LanguageService::native_obligations(self, uri, subject_identity)
    }
    fn native_kind_count(
        &self,
        uri: &str,
        wanted: crate::indexes::SymbolKind,
    ) -> Result<NativeKindCountResponse, ServiceError> {
        LanguageService::native_kind_count(self, uri, wanted)
    }
    fn context_packet(
        &self,
        uri: &str,
        identity: &str,
        max_excerpts: usize,
    ) -> Result<ContextPacketResponse, ServiceError> {
        LanguageService::context_packet(self, uri, identity, max_excerpts)
    }
    fn analyze_candidate(
        &self,
        uri: &str,
        candidate_text: &str,
    ) -> Result<CandidateAnalysisResponse, ServiceError> {
        LanguageService::analyze_candidate(self, uri, candidate_text)
    }
    fn hover(&self, uri: &str, line: u32, character: u32) -> Result<HoverResponse, ServiceError> {
        LanguageService::hover(self, uri, line, character)
    }
    fn highlights(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<HighlightsResponse, ServiceError> {
        LanguageService::highlights(self, uri, line, character)
    }
    fn signature_help(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<SignatureHelpResponse, ServiceError> {
        LanguageService::signature_help(self, uri, line, character)
    }
    fn declaration(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        LanguageService::declaration(self, uri, line, character)
    }
    fn type_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        LanguageService::type_definition(self, uri, line, character)
    }
    fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PrepareCallHierarchyResponse, ServiceError> {
        LanguageService::prepare_call_hierarchy(self, uri, line, character)
    }
    fn incoming_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        LanguageService::incoming_calls(self, uri, identity)
    }
    fn outgoing_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        LanguageService::outgoing_calls(self, uri, identity)
    }
    fn semantic_tokens(&self, uri: &str) -> Result<SemanticTokensResponse, ServiceError> {
        LanguageService::semantic_tokens(self, uri)
    }
    fn folding_ranges(&self, uri: &str) -> Result<FoldingRangesResponse, ServiceError> {
        LanguageService::folding_ranges(self, uri)
    }
    fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<RenameResponse, ServiceError> {
        LanguageService::rename(self, uri, line, character, new_name)
    }
    fn formatting(&self, uri: &str) -> Result<FormattingResponse, ServiceError> {
        LanguageService::formatting(self, uri)
    }
    fn range_formatting(
        &self,
        uri: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<RangeFormattingResponse, ServiceError> {
        LanguageService::range_formatting(self, uri, start_line, end_line)
    }
    fn selection_ranges(
        &self,
        uri: &str,
        positions: &[(u32, u32)],
    ) -> Result<SelectionRangesResponse, ServiceError> {
        LanguageService::selection_ranges(self, uri, positions)
    }
    fn inlay_hints(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<InlayHintsResponse, ServiceError> {
        LanguageService::inlay_hints(
            self,
            uri,
            start_line,
            start_character,
            end_line,
            end_character,
        )
    }
    fn code_actions(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<CodeActionsResponse, ServiceError> {
        LanguageService::code_actions(
            self,
            uri,
            start_line,
            start_character,
            end_line,
            end_character,
        )
    }
    fn completion(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CompletionResponse, ServiceError> {
        LanguageService::completion(self, uri, line, character)
    }
}

#[derive(Debug, Clone)]
pub struct RemoteLanguageService {
    socket: Arc<PathBuf>,
    root: Arc<RwLock<Option<PathBuf>>>,
    /// Persistent connection shared by clones. Calls multiplex over one
    /// Unix socket (one request/response at a time) instead of paying
    /// connect setup per query; a broken connection reconnects once.
    conn: Arc<std::sync::Mutex<Option<std::io::BufReader<std::os::unix::net::UnixStream>>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    connections: Arc<std::sync::atomic::AtomicU64>,
}

impl RemoteLanguageService {
    pub fn connect_path(path: impl AsRef<Path>) -> Self {
        Self {
            socket: Arc::new(path.as_ref().to_path_buf()),
            root: Arc::new(RwLock::new(None)),
            conn: Arc::new(std::sync::Mutex::new(None)),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            connections: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// How many socket connections this client (and its clones) established.
    /// Warm steady state holds this at one per server lifetime.
    pub fn connection_count(&self) -> u64 {
        self.connections.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run several calls in one round trip. Each item resolves or fails
    /// independently; one bad call never poisons the rest.
    pub fn batch(&self, calls: Vec<BatchCall>) -> Result<Vec<BatchResult>, ServiceError> {
        self.call("batch", json!({"calls": calls}))
    }

    fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, ServiceError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))
            .map_err(|error| ServiceError::InvalidRequest {
                reason: error.to_string(),
            })?;
        // One reconnect on transport failure (a restarted server shows up
        // as a broken pipe, not an error response). Logical answers —
        // including failures — return immediately and never resend. A retry
        // after a lost response may re-apply a mutation, but versions make
        // document mutations idempotent in effect.
        let mut guard = self.conn.lock().map_err(|_| ServiceError::InvalidRequest {
            reason: "resident connection state was poisoned".to_owned(),
        })?;
        if guard.is_none() {
            *guard = Some(connect(&self.socket)?);
            self.connections
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let stream = guard.as_mut().expect("connected");
        match call_once(stream, &request, id, method) {
            Ok(value) => Ok(value),
            Err(CallFailure::Logical(error)) => Err(error),
            Err(CallFailure::Transport(_)) => {
                *guard = Some(connect(&self.socket)?);
                self.connections
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let stream = guard.as_mut().expect("reconnected");
                call_once(stream, &request, id, method).map_err(CallFailure::into_error)
            }
        }
    }
}

/// Transport failures (reconnect and retry once) versus logical answers
/// (return as-is, never resend).
enum CallFailure {
    Transport(ServiceError),
    Logical(ServiceError),
}

impl CallFailure {
    fn into_error(self) -> ServiceError {
        match self {
            Self::Transport(error) | Self::Logical(error) => error,
        }
    }
}

fn connect(
    socket: &Path,
) -> Result<std::io::BufReader<std::os::unix::net::UnixStream>, ServiceError> {
    let stream = std::os::unix::net::UnixStream::connect(socket).map_err(|error| {
        ServiceError::WorkspaceUnavailable {
            path: format!("{}: {error}", socket.display()),
        }
    })?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    Ok(std::io::BufReader::new(stream))
}

fn call_once<T: DeserializeOwned>(
    reader: &mut std::io::BufReader<std::os::unix::net::UnixStream>,
    request: &str,
    id: u64,
    method: &str,
) -> Result<T, CallFailure> {
    use std::io::Write;
    let transport =
        |path: String| CallFailure::Transport(ServiceError::WorkspaceUnavailable { path });
    let stream = reader.get_mut();
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| transport(error.to_string()))?;
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| transport(error.to_string()))?;
    if line.is_empty() {
        return Err(transport(
            "resident service closed the connection".to_owned(),
        ));
    }
    let response: RpcResponse = serde_json::from_str(&line).map_err(|error| {
        CallFailure::Logical(ServiceError::InvalidRequest {
            reason: format!("invalid resident response: {error}"),
        })
    })?;
    if response.id != id {
        // The connection serializes requests, so a mismatched id means the
        // stream desynchronized; drop it rather than trusting alignment.
        return Err(transport(format!(
            "resident response id mismatch: expected {id}, got {}",
            response.id
        )));
    }
    if !response.ok {
        return Err(CallFailure::Logical(ServiceError::InvalidRequest {
            reason: response
                .error
                .unwrap_or_else(|| "resident service request failed".to_owned()),
        }));
    }
    serde_json::from_value(response.result.unwrap_or(Value::Null)).map_err(|error| {
        CallFailure::Logical(ServiceError::InvalidRequest {
            reason: format!("invalid resident result for {method}: {error}"),
        })
    })
}

use std::io::{BufRead, Read};

#[derive(Debug, Serialize, Deserialize)]
struct RpcResponse {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

macro_rules! remote_call {
    ($self:ident, $method:literal, $params:expr, $ty:ty) => {
        $self.call::<$ty>($method, $params)
    };
}

impl LanguageServiceClient for RemoteLanguageService {
    fn workspace_root(&self) -> Option<PathBuf> {
        self.root.read().ok().and_then(|root| root.clone())
    }
    fn configure_root(&self, root: Option<PathBuf>) -> Result<Vec<String>, ServiceError> {
        let result = remote_call!(self, "configure_root", json!({"root": root}), Vec::<String>);
        if result.is_ok() {
            if let Ok(mut current) = self.root.write() {
                *current = root;
            }
        }
        result
    }
    fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        remote_call!(
            self,
            "did_open",
            json!({"uri": uri, "version": version, "text": text}),
            u64
        )
    }
    fn did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<TextChange>,
    ) -> Result<u64, ServiceError> {
        remote_call!(
            self,
            "did_change_incremental",
            json!({"uri": uri, "version": version, "changes": changes}),
            u64
        )
    }
    fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError> {
        remote_call!(self, "did_save", json!({"uri": uri, "text": text}), u64)
    }
    fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        remote_call!(self, "did_close", json!({"uri": uri}), Option::<String>)
    }
    fn content(&self, uri: &str) -> Result<String, ServiceError> {
        remote_call!(self, "content", json!({"uri": uri}), String)
    }
    fn buffer_version(&self, uri: &str) -> Result<Option<i32>, ServiceError> {
        remote_call!(self, "buffer_version", json!({"uri": uri}), Option::<i32>)
    }
    fn refresh_workspace(&self) -> Result<Vec<u64>, ServiceError> {
        remote_call!(self, "refresh_workspace", json!({}), Vec::<u64>)
    }
    fn workspace_status(&self) -> Result<WorkspaceStatusResponse, ServiceError> {
        remote_call!(self, "workspace_status", json!({}), WorkspaceStatusResponse)
    }
    fn service_status(&self) -> Result<ServiceStatusResponse, ServiceError> {
        remote_call!(self, "service_status", json!({}), ServiceStatusResponse)
    }
    fn service_stats(&self) -> Result<ServiceStatsSnapshot, ServiceError> {
        remote_call!(self, "service_stats", json!({}), ServiceStatsSnapshot)
    }
    fn semantic_capsule(
        &self,
        known_stream_identity: Option<&str>,
        known_cursor: u64,
    ) -> Result<SemanticCapsuleResponse, ServiceError> {
        remote_call!(
            self,
            "semantic_capsule",
            json!({"known_stream_identity": known_stream_identity, "known_cursor": known_cursor}),
            SemanticCapsuleResponse
        )
    }
    fn poll_events(&self, after_cursor: u64, max_events: usize) -> WorkspaceEventCursor {
        self.call(
            "poll_events",
            json!({"after_cursor": after_cursor, "max_events": max_events}),
        )
        .unwrap_or_else(|error| WorkspaceEventCursor {
            schema_version: crate::WORKSPACE_EVENT_CURSOR_SCHEMA_VERSION.to_owned(),
            stream_identity: String::new(),
            after_cursor,
            current_cursor: after_cursor,
            oldest_cursor: after_cursor.saturating_add(1),
            reset_required: true,
            events: Vec::new(),
            limitations: vec![error.to_string()],
        })
    }
    fn language_capabilities(
        &self,
        topic: Option<&str>,
        symbol: Option<&str>,
        profile: Option<&str>,
        delta_from: Option<&str>,
        known_identity: Option<&str>,
        max_items: usize,
    ) -> Result<LanguageCapabilitiesResponse, ServiceError> {
        remote_call!(
            self,
            "language_capabilities",
            json!({"topic": topic, "symbol": symbol, "profile": profile, "delta_from": delta_from, "known_identity": known_identity, "max_items": max_items}),
            LanguageCapabilitiesResponse
        )
    }
    fn family_agent_context(
        &self,
        repository: Option<&str>,
        topic: Option<&str>,
        symbol: Option<&str>,
        known_language_identity: Option<&str>,
        known_architecture_identity: Option<&str>,
        max_items: usize,
    ) -> Result<FamilyAgentContextResponse, ServiceError> {
        remote_call!(
            self,
            "family_agent_context",
            json!({"repository": repository, "topic": topic, "symbol": symbol, "known_language_identity": known_language_identity, "known_architecture_identity": known_architecture_identity, "max_items": max_items}),
            FamilyAgentContextResponse
        )
    }
    fn document_diagnostics(&self, uri: &str) -> Result<DiagnosticsResponse, ServiceError> {
        remote_call!(
            self,
            "document_diagnostics",
            json!({"uri": uri}),
            DiagnosticsResponse
        )
    }
    fn subjects_at(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PositionQueryResponse, ServiceError> {
        remote_call!(
            self,
            "subjects_at",
            json!({"uri": uri, "line": line, "character": character}),
            PositionQueryResponse
        )
    }
    fn debug_source_binding(
        &self,
        uri: &str,
        identity: Option<&str>,
        line: Option<u32>,
        character: Option<u32>,
    ) -> Result<DebugSourceBindingResponse, ServiceError> {
        remote_call!(
            self,
            "debug_source_binding",
            json!({"uri": uri, "identity": identity, "line": line, "character": character}),
            DebugSourceBindingResponse
        )
    }
    fn describe_identity(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<DescribeResponse, ServiceError> {
        remote_call!(
            self,
            "describe_identity",
            json!({"uri": uri, "identity": identity}),
            DescribeResponse
        )
    }
    fn describe_position(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DescribeResponse, ServiceError> {
        remote_call!(
            self,
            "describe_position",
            json!({"uri": uri, "line": line, "character": character}),
            DescribeResponse
        )
    }
    fn definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        remote_call!(
            self,
            "definition",
            json!({"uri": uri, "line": line, "character": character}),
            DefinitionResponse
        )
    }
    fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResponse, ServiceError> {
        remote_call!(
            self,
            "references",
            json!({"uri": uri, "line": line, "character": character, "include_declaration": include_declaration}),
            ReferencesResponse
        )
    }
    fn document_symbols(&self, uri: &str) -> Result<DocumentSymbolsResponse, ServiceError> {
        remote_call!(
            self,
            "document_symbols",
            json!({"uri": uri}),
            DocumentSymbolsResponse
        )
    }
    fn workspace_symbols(&self, query: &str) -> Result<WorkspaceSymbolsResponse, ServiceError> {
        remote_call!(
            self,
            "workspace_symbols",
            json!({"query": query}),
            WorkspaceSymbolsResponse
        )
    }
    fn dependencies(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        remote_call!(
            self,
            "dependencies",
            json!({"uri": uri, "identity": identity}),
            GraphResponse
        )
    }
    fn dependents(&self, uri: &str, identity: &str) -> Result<GraphResponse, ServiceError> {
        remote_call!(
            self,
            "dependents",
            json!({"uri": uri, "identity": identity}),
            GraphResponse
        )
    }
    fn transitive_dependents(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        remote_call!(
            self,
            "transitive_dependents",
            json!({"uri": uri, "max_depth": max_depth}),
            TransitiveDepsResponse
        )
    }
    fn transitive_dependencies(
        &self,
        uri: &str,
        max_depth: usize,
    ) -> Result<TransitiveDepsResponse, ServiceError> {
        remote_call!(
            self,
            "transitive_dependencies",
            json!({"uri": uri, "max_depth": max_depth}),
            TransitiveDepsResponse
        )
    }
    fn transitive_callers(
        &self,
        identity: &str,
        max_depth: usize,
    ) -> Result<TransitiveCallersResponse, ServiceError> {
        remote_call!(
            self,
            "transitive_callers",
            json!({"identity": identity, "max_depth": max_depth}),
            TransitiveCallersResponse
        )
    }
    fn semantic_impact(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<SemanticImpactResponse, ServiceError> {
        remote_call!(
            self,
            "semantic_impact",
            json!({"uri": uri, "identity": identity}),
            SemanticImpactResponse
        )
    }
    fn obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<crate::queries::ObligationsForUri, ServiceError> {
        remote_call!(
            self,
            "obligations",
            json!({"uri": uri, "subject_identity": subject_identity}),
            crate::queries::ObligationsForUri
        )
    }
    fn native_obligations(
        &self,
        uri: &str,
        subject_identity: Option<&str>,
    ) -> Result<NativeObligationsResponse, ServiceError> {
        remote_call!(
            self,
            "native_obligations",
            json!({"uri": uri, "subject_identity": subject_identity}),
            NativeObligationsResponse
        )
    }
    fn native_kind_count(
        &self,
        uri: &str,
        wanted: crate::indexes::SymbolKind,
    ) -> Result<NativeKindCountResponse, ServiceError> {
        remote_call!(
            self,
            "native_kind_count",
            json!({"uri": uri, "wanted": wanted}),
            NativeKindCountResponse
        )
    }
    fn context_packet(
        &self,
        uri: &str,
        identity: &str,
        max_excerpts: usize,
    ) -> Result<ContextPacketResponse, ServiceError> {
        remote_call!(
            self,
            "context_packet",
            json!({"uri": uri, "identity": identity, "max_excerpts": max_excerpts}),
            ContextPacketResponse
        )
    }
    fn analyze_candidate(
        &self,
        uri: &str,
        candidate_text: &str,
    ) -> Result<CandidateAnalysisResponse, ServiceError> {
        remote_call!(
            self,
            "analyze_candidate",
            json!({"uri": uri, "candidate_text": candidate_text}),
            CandidateAnalysisResponse
        )
    }
    fn hover(&self, uri: &str, line: u32, character: u32) -> Result<HoverResponse, ServiceError> {
        remote_call!(
            self,
            "hover",
            json!({"uri": uri, "line": line, "character": character}),
            HoverResponse
        )
    }
    fn highlights(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<HighlightsResponse, ServiceError> {
        remote_call!(
            self,
            "highlights",
            json!({"uri": uri, "line": line, "character": character}),
            HighlightsResponse
        )
    }
    fn signature_help(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<SignatureHelpResponse, ServiceError> {
        remote_call!(
            self,
            "signature_help",
            json!({"uri": uri, "line": line, "character": character}),
            SignatureHelpResponse
        )
    }
    fn declaration(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        remote_call!(
            self,
            "declaration",
            json!({"uri": uri, "line": line, "character": character}),
            DefinitionResponse
        )
    }
    fn type_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResponse, ServiceError> {
        remote_call!(
            self,
            "type_definition",
            json!({"uri": uri, "line": line, "character": character}),
            DefinitionResponse
        )
    }
    fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<PrepareCallHierarchyResponse, ServiceError> {
        remote_call!(
            self,
            "prepare_call_hierarchy",
            json!({"uri": uri, "line": line, "character": character}),
            PrepareCallHierarchyResponse
        )
    }
    fn incoming_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        remote_call!(
            self,
            "incoming_calls",
            json!({"uri": uri, "identity": identity}),
            CallHierarchyCallsResponse
        )
    }
    fn outgoing_calls(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<CallHierarchyCallsResponse, ServiceError> {
        remote_call!(
            self,
            "outgoing_calls",
            json!({"uri": uri, "identity": identity}),
            CallHierarchyCallsResponse
        )
    }
    fn semantic_tokens(&self, uri: &str) -> Result<SemanticTokensResponse, ServiceError> {
        remote_call!(
            self,
            "semantic_tokens",
            json!({"uri": uri}),
            SemanticTokensResponse
        )
    }
    fn folding_ranges(&self, uri: &str) -> Result<FoldingRangesResponse, ServiceError> {
        remote_call!(
            self,
            "folding_ranges",
            json!({"uri": uri}),
            FoldingRangesResponse
        )
    }
    fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<RenameResponse, ServiceError> {
        remote_call!(
            self,
            "rename",
            json!({"uri": uri, "line": line, "character": character, "new_name": new_name}),
            RenameResponse
        )
    }
    fn formatting(&self, uri: &str) -> Result<FormattingResponse, ServiceError> {
        remote_call!(self, "formatting", json!({"uri": uri}), FormattingResponse)
    }
    fn range_formatting(
        &self,
        uri: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<RangeFormattingResponse, ServiceError> {
        remote_call!(
            self,
            "range_formatting",
            json!({"uri": uri, "start_line": start_line, "end_line": end_line}),
            RangeFormattingResponse
        )
    }
    fn selection_ranges(
        &self,
        uri: &str,
        positions: &[(u32, u32)],
    ) -> Result<SelectionRangesResponse, ServiceError> {
        remote_call!(
            self,
            "selection_ranges",
            json!({"uri": uri, "positions": positions}),
            SelectionRangesResponse
        )
    }
    fn inlay_hints(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<InlayHintsResponse, ServiceError> {
        remote_call!(
            self,
            "inlay_hints",
            json!({"uri": uri, "start_line": start_line, "start_character": start_character, "end_line": end_line, "end_character": end_character}),
            InlayHintsResponse
        )
    }
    fn code_actions(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<CodeActionsResponse, ServiceError> {
        remote_call!(
            self,
            "code_actions",
            json!({"uri": uri, "start_line": start_line, "start_character": start_character, "end_line": end_line, "end_character": end_character}),
            CodeActionsResponse
        )
    }
    fn completion(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CompletionResponse, ServiceError> {
        remote_call!(
            self,
            "completion",
            json!({"uri": uri, "line": line, "character": character}),
            CompletionResponse
        )
    }
}

/// JSON-line request/response host for one canonical resident service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One item of a `batch` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCall {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One item of a `batch` result. Items resolve or fail independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Upper bound on calls accepted in one `batch` request.
pub const MAX_BATCH_CALLS: usize = 256;
/// Upper bound on one framed request line (64 MiB; whole-document
/// payloads are bounded to 4 MiB by the store, batches well below this).
pub const MAX_REQUEST_LINE_BYTES: u64 = 64 * 1024 * 1024;

pub fn serve_unix(path: impl AsRef<Path>, service: Arc<LanguageService>) -> std::io::Result<()> {
    let path = path.as_ref();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    eprintln!(
        "mnls-language-service-host: resident service listening at {}",
        path.display()
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let service = Arc::clone(&service);
        std::thread::spawn(move || handle_connection(stream, service));
    }
    Ok(())
}

fn handle_connection(stream: std::os::unix::net::UnixStream, service: Arc<LanguageService>) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(stream);
    // Persistent framing: many requests per connection, one JSON value per
    // line each way, until the client closes. One-shot clients that send a
    // single line and disconnect keep working unchanged.
    loop {
        let mut raw = Vec::new();
        let read = reader
            .by_ref()
            .take(MAX_REQUEST_LINE_BYTES + 1)
            .read_until(b'\n', &mut raw);
        match read {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }
        if raw.len() as u64 > MAX_REQUEST_LINE_BYTES {
            return;
        }
        let line = String::from_utf8_lossy(&raw);
        if line.trim().is_empty() {
            continue;
        }
        let request: Result<RpcRequest, _> = serde_json::from_str(&line);
        let (id, outcome) = match request {
            Ok(request) => (
                request.id,
                dispatch(&service, &request.method, request.params),
            ),
            Err(error) => (
                0,
                Err(ServiceError::InvalidRequest {
                    reason: error.to_string(),
                }),
            ),
        };
        let response = match outcome {
            Ok(result) => RpcResponse {
                id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => RpcResponse {
                id,
                ok: false,
                result: None,
                error: Some(error.to_string()),
            },
        };
        let mut encoded = serde_json::to_string(&response).unwrap_or_else(|_| {
            "{\"id\":0,\"ok\":false,\"error\":\"response encoding failed\"}".to_owned()
        });
        encoded.push('\n');
        let stream = reader.get_mut();
        if std::io::Write::write_all(stream, encoded.as_bytes()).is_err() {
            return;
        }
    }
}

fn value<T: Serialize>(result: Result<T, ServiceError>) -> Result<Value, ServiceError> {
    result.and_then(|value| {
        serde_json::to_value(value).map_err(|error| ServiceError::InvalidRequest {
            reason: error.to_string(),
        })
    })
}

fn parse<T: DeserializeOwned>(params: &Value, key: &str) -> Result<T, ServiceError> {
    let value = params
        .get(key)
        .cloned()
        .ok_or_else(|| ServiceError::InvalidRequest {
            reason: format!("missing parameter {key}"),
        })?;
    serde_json::from_value(value).map_err(|error| ServiceError::InvalidRequest {
        reason: format!("invalid parameter {key}: {error}"),
    })
}

fn text(params: &Value, key: &str) -> Result<String, ServiceError> {
    parse::<String>(params, key)
}

fn optional<T: DeserializeOwned>(params: &Value, key: &str) -> Result<Option<T>, ServiceError> {
    params
        .get(key)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| ServiceError::InvalidRequest {
            reason: format!("invalid parameter {key}: {error}"),
        })
}

fn dispatch(service: &LanguageService, method: &str, params: Value) -> Result<Value, ServiceError> {
    match method {
        "configure_root" => value(service.configure_root(optional(&params, "root")?)),
        "did_open" => value(service.did_open(
            &text(&params, "uri")?,
            parse(&params, "version")?,
            parse(&params, "text")?,
        )),
        "did_change_incremental" => value(service.did_change_incremental(
            &text(&params, "uri")?,
            parse(&params, "version")?,
            parse(&params, "changes")?,
        )),
        "did_save" => value(service.did_save(&text(&params, "uri")?, optional(&params, "text")?)),
        "did_close" => value(service.did_close(&text(&params, "uri")?)),
        "content" => value(service.content(&text(&params, "uri")?)),
        "buffer_version" => value(service.store().buffer_version(&text(&params, "uri")?)),
        "refresh_workspace" => value(service.refresh_workspace()),
        "workspace_status" => value(service.workspace_status()),
        "service_status" => value(service.service_status()),
        "service_stats" => value(Ok(service.service_stats())),
        "semantic_capsule" => {
            let known: Option<String> = optional(&params, "known_stream_identity")?;
            value(
                service.semantic_capsule(
                    known.as_deref(),
                    params
                        .get("known_cursor")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                ),
            )
        }
        "poll_events" => serde_json::to_value(service.poll_events_for(
            optional::<String>(&params, "stream_identity")?.as_deref(),
            parse(&params, "after_cursor")?,
            parse(&params, "max_events")?,
        ))
        .map_err(|error| ServiceError::InvalidRequest {
            reason: error.to_string(),
        }),
        "language_capabilities" => value(service.language_capabilities(
            optional::<String>(&params, "topic")?.as_deref(),
            optional::<String>(&params, "symbol")?.as_deref(),
            optional::<String>(&params, "profile")?.as_deref(),
            optional::<String>(&params, "delta_from")?.as_deref(),
            optional::<String>(&params, "known_identity")?.as_deref(),
            parse(&params, "max_items")?,
        )),
        "family_agent_context" => value(service.family_agent_context(
            optional::<String>(&params, "repository")?.as_deref(),
            optional::<String>(&params, "topic")?.as_deref(),
            optional::<String>(&params, "symbol")?.as_deref(),
            optional::<String>(&params, "known_language_identity")?.as_deref(),
            optional::<String>(&params, "known_architecture_identity")?.as_deref(),
            parse(&params, "max_items")?,
        )),
        "document_diagnostics" => value(service.document_diagnostics(&text(&params, "uri")?)),
        "subjects_at" => value(service.subjects_at(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "debug_source_binding" => value(service.debug_source_binding(
            &text(&params, "uri")?,
            optional::<String>(&params, "identity")?.as_deref(),
            optional(&params, "line")?,
            optional(&params, "character")?,
        )),
        "describe_identity" => {
            value(service.describe_identity(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "describe_position" => value(service.describe_position(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "definition" => value(service.definition(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "references" => value(service.references(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
            parse(&params, "include_declaration")?,
        )),
        "document_symbols" => value(service.document_symbols(&text(&params, "uri")?)),
        "workspace_symbols" => serde_json::to_value(
            service.workspace_symbols(&text(&params, "query")?),
        )
        .map_err(|error| ServiceError::InvalidRequest {
            reason: error.to_string(),
        }),
        "dependencies" => {
            value(service.dependencies(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "dependents" => {
            value(service.dependents(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "transitive_dependents" => value(
            service.transitive_dependents(&text(&params, "uri")?, parse(&params, "max_depth")?),
        ),
        "transitive_dependencies" => value(
            service.transitive_dependencies(&text(&params, "uri")?, parse(&params, "max_depth")?),
        ),
        "transitive_callers" => value(
            service.transitive_callers(&text(&params, "identity")?, parse(&params, "max_depth")?),
        ),
        "semantic_impact" => {
            value(service.semantic_impact(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "obligations" => value(service.obligations(
            &text(&params, "uri")?,
            optional::<String>(&params, "subject_identity")?.as_deref(),
        )),
        "native_obligations" => value(service.native_obligations(
            &text(&params, "uri")?,
            optional::<String>(&params, "subject_identity")?.as_deref(),
        )),
        "native_kind_count" => {
            value(service.native_kind_count(&text(&params, "uri")?, parse(&params, "wanted")?))
        }
        "context_packet" => value(service.context_packet(
            &text(&params, "uri")?,
            &text(&params, "identity")?,
            parse(&params, "max_excerpts")?,
        )),
        "analyze_candidate" => value(
            service.analyze_candidate(&text(&params, "uri")?, &text(&params, "candidate_text")?),
        ),
        "hover" => value(service.hover(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "highlights" => value(service.highlights(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "signature_help" => value(service.signature_help(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "declaration" => value(service.declaration(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "type_definition" => value(service.type_definition(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "prepare_call_hierarchy" => value(service.prepare_call_hierarchy(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "incoming_calls" => {
            value(service.incoming_calls(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "outgoing_calls" => {
            value(service.outgoing_calls(&text(&params, "uri")?, &text(&params, "identity")?))
        }
        "semantic_tokens" => value(service.semantic_tokens(&text(&params, "uri")?)),
        "folding_ranges" => value(service.folding_ranges(&text(&params, "uri")?)),
        "rename" => value(service.rename(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
            &text(&params, "new_name")?,
        )),
        "formatting" => value(service.formatting(&text(&params, "uri")?)),
        "range_formatting" => value(service.range_formatting(
            &text(&params, "uri")?,
            parse(&params, "start_line")?,
            parse(&params, "end_line")?,
        )),
        "selection_ranges" => value(service.selection_ranges(
            &text(&params, "uri")?,
            &parse::<Vec<(u32, u32)>>(&params, "positions")?,
        )),
        "inlay_hints" => value(service.inlay_hints(
            &text(&params, "uri")?,
            parse(&params, "start_line")?,
            parse(&params, "start_character")?,
            parse(&params, "end_line")?,
            parse(&params, "end_character")?,
        )),
        "code_actions" => value(service.code_actions(
            &text(&params, "uri")?,
            parse(&params, "start_line")?,
            parse(&params, "start_character")?,
            parse(&params, "end_line")?,
            parse(&params, "end_character")?,
        )),
        "completion" => value(service.completion(
            &text(&params, "uri")?,
            parse(&params, "line")?,
            parse(&params, "character")?,
        )),
        "batch" => {
            let calls: Vec<BatchCall> = parse(&params, "calls")?;
            if calls.len() > MAX_BATCH_CALLS {
                return Err(ServiceError::InvalidRequest {
                    reason: format!(
                        "batch of {} calls exceeds the limit of {MAX_BATCH_CALLS}",
                        calls.len()
                    ),
                });
            }
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                // Batching `batch` nests pointlessly; reject it explicitly
                // rather than recursing.
                if call.method == "batch" {
                    results.push(BatchResult {
                        id: call.id,
                        ok: false,
                        result: None,
                        error: Some("nested batch calls are not supported".to_owned()),
                    });
                    continue;
                }
                match dispatch(service, &call.method, call.params) {
                    Ok(result) => results.push(BatchResult {
                        id: call.id,
                        ok: true,
                        result: Some(result),
                        error: None,
                    }),
                    Err(error) => results.push(BatchResult {
                        id: call.id,
                        ok: false,
                        result: None,
                        error: Some(error.to_string()),
                    }),
                }
            }
            serde_json::to_value(results).map_err(|error| ServiceError::InvalidRequest {
                reason: error.to_string(),
            })
        }
        other => Err(ServiceError::Unsupported {
            reason: format!("unknown resident method {other}"),
        }),
    }
}
