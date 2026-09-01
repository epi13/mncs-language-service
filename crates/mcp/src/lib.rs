//! MNCS Language Service MCP adapter library.
//!
//! Read-only semantic tools over the same resident [`LanguageService`] that
//! backs the LSP adapter. This server intentionally exposes semantic
//! operations only — it is not a filesystem or shell server.

use std::path::PathBuf;
use std::sync::Arc;

use mncs_service_core::LanguageService;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

/// Shared request parameters.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocumentParams {
    /// URI of an MNCS source document known to the service.
    pub uri: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PositionParams {
    /// URI of an MNCS source document known to the service.
    pub uri: String,
    /// Zero-based line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit column.
    pub character: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReferencesParams {
    pub uri: String,
    pub line: u32,
    pub character: u32,
    /// Include the declaration site among the hits (default true).
    #[serde(default = "default_true")]
    pub include_declaration: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolsParams {
    /// Restrict to one document when provided.
    #[serde(default)]
    pub uri: Option<String>,
    /// Substring filter applied case-insensitively.
    #[serde(default)]
    pub name_filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DependenciesParams {
    pub uri: String,
    pub identity: String,
    /// `outgoing` (what this calls) or `incoming` (who calls this).
    #[serde(default = "default_direction")]
    pub direction: String,
}

fn default_direction() -> String {
    "outgoing".to_owned()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObligationsParams {
    pub uri: String,
    /// Filter to one semantic subject (and its dependencies).
    #[serde(default)]
    pub subject_identity: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextPacketParams {
    pub uri: String,
    pub identity: String,
    /// Maximum number of source excerpts included (1..=12).
    #[serde(default = "default_budget")]
    pub max_excerpts: u32,
}

fn default_budget() -> u32 {
    6
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CandidateParams {
    /// URI of the baseline MNCS source document known to the service.
    pub uri: String,
    /// Proposed full document content, analyzed in isolation; the workspace
    /// baseline is never modified.
    pub candidate_text: String,
}

/// The resident MNCS semantic service exposed through MCP.
#[derive(Clone)]
pub struct MncsSemanticServer {
    service: Arc<LanguageService>,
    root: Option<PathBuf>,
    tool_router: ToolRouter<MncsSemanticServer>,
}

fn serialize<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("response serialization cannot fail")
}

fn summary_line(value: &serde_json::Value) -> String {
    let kind = value
        .get("status")
        .and_then(|status| status.get("kind"))
        .and_then(|kind| kind.as_str())
        .unwrap_or("answered");
    format!("MNCS service result ({kind}); structured content carries full data.")
}

#[tool_router]
impl MncsSemanticServer {
    pub fn new(service: Arc<LanguageService>) -> Self {
        Self {
            service,
            root: None,
            tool_router: Self::tool_router(),
        }
    }

    fn answered(&self, value: serde_json::Value) -> CallToolResult {
        let mut result = CallToolResult::success(vec![ContentBlock::text(summary_line(&value))]);
        result.structured_content = Some(value);
        result
    }

    fn failed(&self, message: String) -> CallToolResult {
        let payload = serde_json::json!({ "status": { "kind": "error" }, "message": message });
        let mut result = CallToolResult::error(vec![ContentBlock::text(message)]);
        result.structured_content = Some(payload);
        result
    }

    fn service(&self) -> &LanguageService {
        &self.service
    }

    /// Resolve a possibly-relative document URI against the workspace root.
    fn resolve_uri(&self, uri: &str) -> String {
        if uri.starts_with("file://") || PathBuf::from(uri).is_absolute() || self.root.is_none() {
            return uri.to_owned();
        }
        let joined = self.root.as_ref().expect("checked").join(uri);
        match joined.canonicalize() {
            Ok(canonical) => format!("file://{}", canonical.display()),
            Err(_) => format!("file://{}", joined.display()),
        }
    }

    /// Attach a workspace root once at startup/initialization.
    pub fn set_root(&mut self, root: Option<PathBuf>) -> Result<usize, String> {
        self.root = root.clone();
        let documents = self
            .service
            .configure_root(root)
            .map_err(|error| error.to_string())?;
        Ok(documents.len())
    }

    #[tool(
        description = "Report workspace status: root, generation, every known MNCS document with open/buffer state, analysis currency, validity, and diagnostic counts."
    )]
    async fn workspace_status(&self) -> Result<CallToolResult, McpError> {
        let status = self.service().workspace_status();
        Ok(self.answered(serialize(&status)))
    }

    #[tool(
        description = "Return authoritative diagnostics (code, stage, severity, message, dual-coordinate range, expected/found tokens) for one document."
    )]
    async fn document_diagnostics(
        &self,
        Parameters(DocumentParams { uri }): Parameters<DocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self.service().document_diagnostics(&uri) {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "Resolve the exact semantic subject(s) at a source position. Returns declaration/reference roles, resolved targets, and authoritative identities. Unresolved positions are reported explicitly."
    )]
    async fn identity_at_position(
        &self,
        Parameters(PositionParams {
            uri,
            line,
            character,
        }): Parameters<PositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self.service().subjects_at(&uri, line, character) {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "Describe one semantic subject by identity or position: kind, name, span, type/signature, contracts, effects, capabilities, evidence, related obligations with PASS/FAIL/UNKNOWN status, call-graph neighborhood, and structural members."
    )]
    async fn describe_subject(
        &self,
        Parameters(params): Parameters<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        let uri = params.get("uri").and_then(|value| value.as_str());
        let identity = params.get("identity").and_then(|value| value.as_str());
        let line = params.get("line").and_then(|value| value.as_u64());
        let character = params.get("character").and_then(|value| value.as_u64());
        let outcome = match (uri, identity, line, character) {
            (Some(uri), Some(identity), _, _) => self
                .service()
                .describe_identity(uri, identity)
                .map(|response| serialize(&response)),
            (Some(uri), None, Some(line), Some(character)) => self
                .service()
                .describe_position(uri, line as u32, character as u32)
                .map(|response| serialize(&response)),
            _ => Err(mncs_service_core::ServiceError::InvalidRequest {
                reason: "describe_subject requires uri plus either identity or line+character"
                    .to_owned(),
            }),
        };
        match outcome {
            Ok(value) => Ok(self.answered(value)),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(description = "Find the declaration site(s) a position resolves to.")]
    async fn find_definition(
        &self,
        Parameters(PositionParams {
            uri,
            line,
            character,
        }): Parameters<PositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self.service().definition(&uri, line, character) {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "Find confident references to the symbol at a position. Results come from authoritative name resolution, never text search."
    )]
    async fn find_references(
        &self,
        Parameters(ReferencesParams {
            uri,
            line,
            character,
            include_declaration,
        }): Parameters<ReferencesParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self
            .service()
            .references(&uri, line, character, include_declaration)
        {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "List indexed symbols across the workspace, optionally filtered by document and name substring."
    )]
    async fn list_symbols(
        &self,
        Parameters(SymbolsParams { uri, name_filter }): Parameters<SymbolsParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(uri) = &uri {
            let uri = self.resolve_uri(uri);
            return match self.service().document_symbols(&uri) {
                Ok(response) => Ok(self.answered(serialize(&response))),
                Err(error) => Ok(self.failed(error.to_string())),
            };
        }
        let response = self
            .service()
            .workspace_symbols(name_filter.as_deref().unwrap_or(""));
        Ok(self.answered(serialize(&response)))
    }

    #[tool(
        description = "Function-level dependency query derived from authoritative call operations. direction is 'outgoing' (callees) or 'incoming' (callers)."
    )]
    async fn semantic_dependencies(
        &self,
        Parameters(DependenciesParams {
            uri,
            identity,
            direction,
        }): Parameters<DependenciesParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        let outcome = match direction.as_str() {
            "outgoing" => self.service().dependencies(&uri, &identity),
            "incoming" => self.service().dependents(&uri, &identity),
            other => {
                return Ok(self.failed(format!(
                    "invalid direction {other:?}; expected 'outgoing' or 'incoming'"
                )))
            }
        };
        match outcome {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "Obligations generated by the language for this snapshot, preserving PASS/FAIL/UNKNOWN status, method, freshness, and fallback information."
    )]
    async fn obligations(
        &self,
        Parameters(ObligationsParams {
            uri,
            subject_identity,
        }): Parameters<ObligationsParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self
            .service()
            .obligations(&uri, subject_identity.as_deref())
        {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "EXPERIMENTAL MNCS-native bounded obligation summary. Projects the authoritative PASS/FAIL/UNKNOWN statuses into a bounded MNCS query, executes the real mncs-language research-bytecode backend, and compares its counts with the Rust control result. Unsupported or inconsistent results fail closed."
    )]
    async fn native_obligations(
        &self,
        Parameters(ObligationsParams {
            uri,
            subject_identity,
        }): Parameters<ObligationsParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self
            .service()
            .native_obligations(&uri, subject_identity.as_deref())
        {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "EXPERIMENTAL bounded semantic context packet around a subject: its declaration excerpt plus callee excerpts within budget. 'complete' is true only when the whole outgoing-call closure fit in the budget."
    )]
    async fn context_packet(
        &self,
        Parameters(ContextPacketParams {
            uri,
            identity,
            max_excerpts,
        }): Parameters<ContextPacketParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self
            .service()
            .context_packet(&uri, &identity, max_excerpts as usize)
        {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }

    #[tool(
        description = "Analyze proposed document content as an isolated candidate against the resident baseline without modifying the workspace. Returns identity-bound semantic, obligation, and diagnostics deltas plus language-computed stale evidence."
    )]
    async fn analyze_candidate(
        &self,
        Parameters(CandidateParams {
            uri,
            candidate_text,
        }): Parameters<CandidateParams>,
    ) -> Result<CallToolResult, McpError> {
        let uri = self.resolve_uri(&uri);
        match self.service().analyze_candidate(&uri, &candidate_text) {
            Ok(response) => Ok(self.answered(serialize(&response))),
            Err(error) => Ok(self.failed(error.to_string())),
        }
    }
}

#[tool_handler(router = self.tool_router.clone())]
impl ServerHandler for MncsSemanticServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info = rmcp::model::ServerInfo::default();
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        info.server_info = rmcp::model::Implementation::from_build_env();
        info.instructions = Some(
            "Read-only semantic inspection of an MNCS workspace. Prefer these tools over \
             reading raw files: resolve subjects by identity, describe semantics, follow \
             dependencies, and consult obligation/evidence state. All results are bound to \
             an analysis snapshot identified by its authoritative source identity."
                .to_owned(),
        );
        info
    }
}

use rmcp::handler::server::wrapper::Parameters;

/// Serve the MNCS semantic tools over stdio until the peer disconnects.
///
/// The workspace root comes from `MNLS_WORKSPACE_ROOT`; otherwise the current
/// directory is used when it contains `.mncs` files nearby.
pub async fn serve_stdio(mut server: MncsSemanticServer) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(root) = std::env::var_os("MNLS_WORKSPACE_ROOT").map(PathBuf::from) {
        server
            .set_root(Some(root))
            .map_err(|error| error.as_str().to_owned())?;
    } else if let Ok(cwd) = std::env::current_dir() {
        if has_mncs_files(&cwd, 2) {
            let _ = server.set_root(Some(cwd));
        }
    }

    let transport = rmcp::transport::io::stdio();
    let running = rmcp::serve_server(server, transport).await?;
    running.waiting().await?;
    Ok(())
}

fn has_mncs_files(directory: &std::path::Path, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "mncs")
        {
            return true;
        }
        if path.is_dir() && has_mncs_files(&path, depth - 1) {
            return true;
        }
    }
    false
}
