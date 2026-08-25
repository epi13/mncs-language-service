//! MNCS Language Service LSP adapter library.
//!
//! The protocol-facing backend is deliberately small: it translates between
//! LSP types and the shared [`mncs_service_core`] queries that also serve the
//! MCP adapter.

//! LSP adapter for the MNCS Language Service.
//!
//! A thin projection of [`mncs_service_core`] onto the Language Server
//! Protocol. It owns no language semantics: every request is answered by the
//! same resident core that backs the MCP adapter.

use std::path::PathBuf;
use std::sync::Arc;

use mncs_service_core::{
    CompletionClass, LanguageService, ResponseStatus, SymbolKind as CoreSymbolKind, TokenClass,
};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use url::Url as Uri;

/// Semantic token legend order; indices must match `token_type_index`.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::PARAMETER,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::TYPE,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::KEYWORD,
    SemanticTokenType::NUMBER,
];

fn token_type_index(class: TokenClass) -> u32 {
    match class {
        TokenClass::Module => 0,
        TokenClass::Function => 1,
        TokenClass::Parameter => 2,
        TokenClass::Variable => 3,
        TokenClass::Type => 4,
        TokenClass::Variant => 5,
        TokenClass::Field => 6,
        TokenClass::Keyword => 7,
        TokenClass::Number => 8,
    }
}

pub struct Backend {
    client: Client,
    service: Arc<LanguageService>,
}

impl Backend {
    fn to_range(range: mncs_service_core::RangeInfo) -> Range {
        Range::new(
            Position::new(range.start_line, range.start_character),
            Position::new(range.end_line, range.end_character),
        )
    }

    /// Send a server→client message without blocking request handling: the
    /// underlying channel only drains while someone polls the socket, so
    /// awaiting these inline can stall notification handling.
    fn spawn_client_message(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        tokio::spawn(future);
    }

    fn publish_diagnostics(&self, uri: &Uri) {
        let diagnostics = match self.service.document_diagnostics(uri.as_str()) {
            Ok(response) => response
                .items
                .iter()
                .map(|item| Diagnostic {
                    range: Self::to_range(item.range),
                    severity: Some(if item.severity == "error" {
                        DiagnosticSeverity::ERROR
                    } else {
                        DiagnosticSeverity::WARNING
                    }),
                    code: Some(NumberOrString::String(item.code.clone())),
                    code_description: None,
                    source: Some("mncs".to_owned()),
                    message: item.message.clone(),
                    related_information: None,
                    tags: None,
                    // Structured metadata without requiring client support.
                    data: Some(serde_json::json!({
                        "stage": item.stage,
                        "expected": item.expected,
                        "found": item.found,
                    })),
                })
                .collect(),
            Err(error) => vec![Diagnostic {
                range: Range::default(),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String("MNCSVC001".to_owned())),
                code_description: None,
                source: Some("mncs".to_owned()),
                message: format!("analysis unavailable: {error}"),
                related_information: None,
                tags: None,
                data: None,
            }],
        };
        let version = self
            .service
            .store()
            .buffer_version(uri.as_str())
            .ok()
            .flatten();
        let client = self.client.clone();
        let owned_uri = uri.clone();
        self.spawn_client_message(async move {
            client
                .publish_diagnostics(owned_uri, diagnostics, version)
                .await;
        });
    }

    fn log_unanswered(&self, status: &ResponseStatus) {
        let reason = match status {
            ResponseStatus::Unsupported { reason } | ResponseStatus::Unresolved { reason } => {
                reason.clone()
            }
            ResponseStatus::Answered => return,
        };
        {
            let client = self.client.clone();
            self.spawn_client_message(async move {
                client.log_message(MessageType::INFO, reason).await;
            });
        }
    }

    fn location(&self, summary: &mncs_service_core::SymbolSummary) -> Option<Location> {
        let uri_text = summary.uri.as_deref()?;
        let uri = Uri::parse(uri_text).ok()?;
        Some(Location::new(uri, Self::to_range(summary.name_range)))
    }
}

fn symbol_kind(kind: CoreSymbolKind) -> SymbolKind {
    match kind {
        CoreSymbolKind::Module => SymbolKind::MODULE,
        CoreSymbolKind::Function => SymbolKind::FUNCTION,
        CoreSymbolKind::Parameter | CoreSymbolKind::Binding | CoreSymbolKind::IterationState => {
            SymbolKind::VARIABLE
        }
        CoreSymbolKind::FiniteType => SymbolKind::ENUM,
        CoreSymbolKind::FiniteVariant => SymbolKind::ENUM_MEMBER,
        CoreSymbolKind::RecordType => SymbolKind::STRUCT,
        CoreSymbolKind::RecordField => SymbolKind::FIELD,
    }
}

fn completion_item_kind(class: &CompletionClass) -> Option<CompletionItemKind> {
    Some(match class {
        CompletionClass::Symbol(CoreSymbolKind::Function) => CompletionItemKind::FUNCTION,
        CompletionClass::Variable | CompletionClass::Symbol(CoreSymbolKind::Parameter) => {
            CompletionItemKind::VARIABLE
        }
        CompletionClass::BuiltinType | CompletionClass::Symbol(CoreSymbolKind::FiniteType) => {
            CompletionItemKind::CLASS
        }
        CompletionClass::Symbol(CoreSymbolKind::RecordType) => CompletionItemKind::STRUCT,
        CompletionClass::Symbol(CoreSymbolKind::FiniteVariant) => CompletionItemKind::ENUM_MEMBER,
        CompletionClass::Symbol(CoreSymbolKind::RecordField) => CompletionItemKind::FIELD,
        CompletionClass::Symbol(_) => CompletionItemKind::TEXT,
        CompletionClass::Keyword => CompletionItemKind::KEYWORD,
    })
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path().ok())
            .or_else(|| {
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path().ok())
            });

        match self.service.configure_root(root) {
            Ok(documents) => {
                let client = self.client.clone();
                let message = format!("workspace ready with {} MNCS documents", documents.len());
                self.spawn_client_message(async move {
                    client.log_message(MessageType::INFO, message).await;
                });
            }
            Err(error) => {
                let client = self.client.clone();
                let message = format!("workspace discovery skipped: {error}");
                self.spawn_client_message(async move {
                    client.log_message(MessageType::WARNING, message).await;
                });
            }
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![":".to_owned(), ".".to_owned()]),
                    all_commit_characters: None,
                    completion_item: None,
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: WorkDoneProgressOptions::default(),
                            legend: SemanticTokensLegend {
                                token_types: TOKEN_TYPES.to_vec(),
                                token_modifiers: Vec::new(),
                            },
                            range: Some(false),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    ),
                ),
                experimental: Some(serde_json::json!({
                    "mncs/status": true,
                    "mncs/describePosition": true,
                    "mncs/obligations": true,
                })),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "mncs-language-service".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        // Intentionally no client interaction: some transports deliver
        // notifications before clients read server messages.
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Err(error) = self.service.did_open(
            uri.as_str(),
            params.text_document.version,
            params.text_document.text,
        ) {
            let client = self.client.clone();
            let message = format!("{error}");
            self.spawn_client_message(async move {
                client.log_message(MessageType::ERROR, message).await;
            });
            return;
        }
        self.publish_diagnostics(&uri);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(last) = params.content_changes.into_iter().last() else {
            return;
        };
        if let Err(error) =
            self.service
                .did_change(uri.as_str(), params.text_document.version, last.text)
        {
            let client = self.client.clone();
            let message = format!("{error}");
            self.spawn_client_message(async move {
                client.log_message(MessageType::ERROR, message).await;
            });
            return;
        }
        self.publish_diagnostics(&uri);
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        let _ = self.service.did_save(uri.as_str(), params.text);
        self.publish_diagnostics(&uri);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let _ = self.service.did_close(uri.as_str());
        self.publish_diagnostics(&uri);
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let position = params.text_document_position_params.position;
        let uri = params.text_document_position_params.text_document.uri;
        match self
            .service
            .hover(uri.as_str(), position.line, position.character)
        {
            Ok(response) if matches!(response.status, ResponseStatus::Answered) => {
                Ok(Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: response.markdown.unwrap_or_default(),
                    }),
                    range: response
                        .subject
                        .as_ref()
                        .map(|subject| Self::to_range(subject.name_range)),
                }))
            }
            Ok(response) => {
                self.log_unanswered(&response.status);
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params.position;
        let uri = params.text_document_position_params.text_document.uri;
        match self
            .service
            .definition(uri.as_str(), position.line, position.character)
        {
            Ok(response) if matches!(response.status, ResponseStatus::Answered) => {
                Ok(Some(GotoDefinitionResponse::Array(
                    response
                        .definitions
                        .iter()
                        .filter_map(|summary| self.location(summary))
                        .collect(),
                )))
            }
            Ok(response) => {
                self.log_unanswered(&response.status);
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let position = params.text_document_position.position;
        let uri = params.text_document_position.text_document.uri;
        match self.service.references(
            uri.as_str(),
            position.line,
            position.character,
            params.context.include_declaration,
        ) {
            Ok(response) if matches!(response.status, ResponseStatus::Answered) => Ok(Some(
                response
                    .hits
                    .iter()
                    .map(|hit| {
                        let hit_uri = Uri::parse(&hit.uri).unwrap_or_else(|_| uri.clone());
                        Location::new(hit_uri, Self::to_range(hit.range))
                    })
                    .collect(),
            )),
            Ok(response) => {
                self.log_unanswered(&response.status);
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let position = params.text_document_position_params.position;
        let uri = params.text_document_position_params.text_document.uri;
        match self
            .service
            .highlights(uri.as_str(), position.line, position.character)
        {
            Ok(response) if matches!(response.status, ResponseStatus::Answered) => Ok(Some(
                response
                    .ranges
                    .iter()
                    .map(|range| DocumentHighlight {
                        range: Self::to_range(*range),
                        kind: None,
                    })
                    .collect(),
            )),
            Ok(_) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        match self.service.document_symbols(uri.as_str()) {
            Ok(response) if matches!(response.status, ResponseStatus::Answered) => {
                fn convert(node: &mncs_service_core::DocumentSymbolNode) -> DocumentSymbol {
                    DocumentSymbol {
                        name: node.summary.name.clone(),
                        detail: node.summary.detail.clone(),
                        kind: symbol_kind(node.summary.kind),
                        tags: None,
                        #[allow(deprecated)]
                        deprecated: None,
                        range: Backend::to_range(node.summary.range),
                        selection_range: Backend::to_range(node.summary.name_range),
                        children: if node.children.is_empty() {
                            None
                        } else {
                            Some(node.children.iter().map(convert).collect())
                        },
                    }
                }
                Ok(Some(DocumentSymbolResponse::Nested(
                    response.symbols.iter().map(convert).collect(),
                )))
            }
            Ok(_) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let response = self.service.workspace_symbols(&params.query);
        Ok(Some(
            response
                .symbols
                .iter()
                .filter_map(|hit| {
                    Some(SymbolInformation {
                        name: hit.summary.name.clone(),
                        kind: symbol_kind(hit.summary.kind),
                        tags: None,
                        #[allow(deprecated)]
                        deprecated: None,
                        location: self.location(&hit.summary)?,
                        container_name: hit.summary.container.clone(),
                    })
                })
                .collect(),
        ))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Ok(response) = self.service.semantic_tokens(uri.as_str()) else {
            return Ok(None);
        };
        let mut builder = TokenBuilder::default();
        for token in &response.tokens {
            builder.push(
                token.start_line,
                token.start_character,
                token.length_utf16,
                token_type_index(token.class),
            );
        }
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: builder.finish_tokens(),
        })))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let position = params.text_document_position.position;
        let uri = params.text_document_position.text_document.uri;
        let Ok(response) = self
            .service
            .completion(uri.as_str(), position.line, position.character)
        else {
            return Ok(None);
        };
        Ok(Some(CompletionResponse::Array(
            response
                .items
                .into_iter()
                .map(|candidate| CompletionItem {
                    label: candidate.label,
                    kind: completion_item_kind(&candidate.class),
                    detail: candidate.detail,
                    ..CompletionItem::default()
                })
                .collect(),
        )))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        let Ok(response) = self.service.folding_ranges(uri.as_str()) else {
            return Ok(None);
        };
        Ok(Some(
            response
                .ranges
                .iter()
                .map(|range| FoldingRange {
                    start_line: range.start_line,
                    start_character: None,
                    end_line: range.end_line,
                    end_character: None,
                    kind: None,
                    collapsed_text: None,
                })
                .collect(),
        ))
    }
}

#[derive(Default)]
struct TokenBuilder {
    last_line: u32,
    last_start: u32,
    data: Vec<u32>,
    started: bool,
}

impl TokenBuilder {
    fn push(&mut self, line: u32, start: u32, length: u32, token_type: u32) {
        let delta_line = if self.started {
            line - self.last_line
        } else {
            0
        };
        let delta_start = if self.started && delta_line == 0 {
            start - self.last_start
        } else {
            start
        };
        self.data
            .extend([delta_line, delta_start, length, token_type, 0]);
        self.last_line = line;
        self.last_start = start;
        self.started = true;
    }

    fn finish_tokens(self) -> Vec<SemanticToken> {
        self.data
            .chunks_exact(5)
            .map(|chunk| SemanticToken {
                delta_line: chunk[0],
                delta_start: chunk[1],
                length: chunk[2],
                token_type: chunk[3],
                token_modifiers_bitset: chunk[4],
            })
            .collect()
    }
}

/// Run the MNCS language server over stdio until the client disconnects.
pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = create_service(workspace_from_env());
    Server::new(stdin, stdout, socket).serve(service).await;
}

/// Construct an [`LspService`] hosting the MNCS backend over one resident core
/// rooted at `workspace` (when provided).
pub fn create_service(
    workspace: Option<PathBuf>,
) -> (LspService<Backend>, tower_lsp::ClientSocket) {
    LspService::build(|client| Backend {
        client,
        service: Arc::new(LanguageService::new(workspace)),
    })
    .finish()
}

fn workspace_from_env() -> Option<PathBuf> {
    std::env::var_os("MNLS_WORKSPACE_ROOT").map(PathBuf::from)
}
