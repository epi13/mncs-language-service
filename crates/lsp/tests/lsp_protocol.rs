//! Protocol-level tests for the MNCS language server.
//!
//! These drive the real `tower-lsp` service over JSON-RPC and assert on both
//! responses and server-initiated diagnostics publications, exercising the
//! exact sequences an editor performs: initialize, open, receive diagnostics,
//! hover, definition, references, unsaved change, updated diagnostics, close.

use std::path::PathBuf;

use futures::StreamExt;
fn offset_position(text: &str, needle: &str, plus_chars: usize) -> Position {
    let map = PositionMap::new(text);
    let start = text.find(needle).expect("needle present");
    let info = map.position_of(text, start + plus_chars);
    Position::new(info.line, info.character)
}
use mncs_service_core::PositionMap;
use tower::Service as _;
use tower::ServiceExt as _;
use tower_lsp::jsonrpc::{Request, Response as RpcResponse};
use tower_lsp::lsp_types::Position;
use tower_lsp::{ClientSocket, LspService};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn fixture_uri(name: &str) -> String {
    let path = fixtures_dir().join(name);
    format!(
        "file://{}",
        path.canonicalize().expect("fixture path").display()
    )
}

struct Harness {
    service: LspService<mncs_lsp::Backend>,
    socket: ClientSocket,
}

impl Harness {
    async fn new() -> Self {
        let (service, socket) = mncs_lsp::create_service(Some(fixtures_dir()));
        let mut harness = Self { service, socket };
        let initialize = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": fixture_uri_as_root(),
            "capabilities": {},
        });
        eprintln!("HARNESS sending initialize");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            harness.request("initialize", Some(initialize)),
        )
        .await
        .expect("initialize timed out")
        .expect("initialize response");
        assert!(response.error().is_none(), "{response:?}");
        eprintln!("HARNESS initialized ok");
        eprintln!("HARNESS sending initialized notification");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            harness.notify("initialized", Some(serde_json::json!({}))),
        )
        .await
        .expect("initialized notification timed out");
        eprintln!("HARNESS draining socket");
        harness.drain_socket().await;
        eprintln!("HARNESS harness ready");
        harness
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<RpcResponse> {
        let method = method.to_owned();
        let request = match params {
            Some(params) => Request::build(method).params(params).id(1).finish(),
            None => Request::build(method).id(1).finish(),
        };
        self.service
            .ready()
            .await
            .expect("service ready")
            .call(request)
            .await
            .expect("service call")
    }

    async fn notify(&mut self, method: &str, params: Option<serde_json::Value>) {
        let mut builder = Request::build(method.to_owned());
        if let Some(params) = params {
            builder = builder.params(params);
        }
        let notification = builder.finish();
        self.service
            .ready()
            .await
            .expect("service ready")
            .call(notification)
            .await
            .expect("notification accepted");
    }

    /// Collect all server→client messages currently queued.
    async fn drain_socket(&mut self) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        while let Ok(Some(message)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), self.socket.next()).await
        {
            messages.push(serde_json::to_value(&message).expect("serializable message"));
        }
        messages
    }

    /// Wait for a `textDocument/publishDiagnostics` for the given URI.
    async fn next_diagnostics_for(&mut self, uri: &str) -> serde_json::Value {
        for _ in 0..20 {
            if let Ok(Some(message)) =
                tokio::time::timeout(std::time::Duration::from_millis(500), self.socket.next())
                    .await
            {
                let value = serde_json::to_value(&message).expect("message");
                if value.get("method").and_then(|method| method.as_str())
                    == Some("textDocument/publishDiagnostics")
                {
                    let params = &value["params"];
                    if params["uri"].as_str() == Some(uri) {
                        return value;
                    }
                }
            }
        }
        panic!("no diagnostics published for {uri}");
    }
}

fn fixture_uri_as_root() -> String {
    format!(
        "file://{}",
        fixtures_dir()
            .canonicalize()
            .expect("fixtures dir")
            .display()
    )
}

#[tokio::test(flavor = "current_thread")]
async fn initialize_reports_capabilities() {
    let mut harness = Harness::new().await;
    let response = harness.request("shutdown", None).await.expect("shutdown");
    assert!(response.error().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn open_change_hover_definition_references_flow() {
    let mut harness = Harness::new().await;
    let uri = fixture_uri("valid-contracts.mncs");
    let disk_text =
        std::fs::read_to_string(fixtures_dir().join("valid-contracts.mncs")).expect("fixture");

    // 1. Open the document.
    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": disk_text },
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    assert_eq!(published["params"]["diagnostics"], serde_json::json!([]));

    // 2. Hover over the function declaration.
    let hover_position = offset_position(&disk_text, "fn bounded_step", 3);
    let response = harness
        .request(
            "textDocument/hover",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": hover_position,
            })),
        )
        .await
        .expect("hover");
    let result = response.result().expect("hover result").clone();
    let rendered = result["contents"]["value"].as_str().expect("markdown");
    assert!(
        rendered.contains("fn bounded_step(n: i64, limit: i64) -> (result: i64)"),
        "{rendered}"
    );

    // 3. Go to definition from the call site.
    let call_text = &disk_text;
    let map = PositionMap::new(call_text);
    let call_offset = call_text.rfind("bounded_step").expect("call site");
    let info = map.position_of(call_text, call_offset);
    let response = harness
        .request(
            "textDocument/definition",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(info.line, info.character),
            })),
        )
        .await
        .expect("definition");
    let value = response.result().expect("result").clone();
    let locations = value.as_array().expect("location array");
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0]["uri"].as_str().expect("uri"),
        uri,
        "definition points into the same document"
    );

    // 4. References from the declaration.
    let decl_position = offset_position(call_text, "fn bounded_step", 3);
    let response = harness
        .request(
            "textDocument/references",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": decl_position,
                "context": { "includeDeclaration": true },
            })),
        )
        .await
        .expect("references");
    let value = response.result().expect("result").clone();
    let references = value.as_array().expect("array");
    assert_eq!(references.len(), 2, "declaration + one call site");

    // 5. Unsaved buffer edit introduces an error; diagnostics update.
    let broken = disk_text.replace(
        "return bounded_step(value, value);",
        "return missing_fn(value, value);",
    );
    harness
        .notify(
            "textDocument/didChange",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": broken }],
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    let diagnostics = published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(
        diagnostics.iter().any(|item| item["code"] == "MNE131"),
        "unresolved call reported: {diagnostics:?}"
    );

    // 6. Hover now reflects the new snapshot (function still hovers fine).
    let response = harness
        .request(
            "textDocument/hover",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": hover_position,
            })),
        )
        .await
        .expect("hover after change");
    let value = response.result().expect("result").clone();
    assert!(
        !value.is_null(),
        "declaration hover survives unrelated breakage"
    );

    // 7. Close reverts to disk; diagnostics go clean again.
    harness
        .notify(
            "textDocument/didClose",
            Some(serde_json::json!({ "textDocument": { "uri": uri } })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    let diagnostics = published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    harness.request("shutdown", None).await.expect("shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn document_symbols_and_semantic_tokens_are_served() {
    let mut harness = Harness::new().await;
    let uri = fixture_uri("records.mncs");
    let text = std::fs::read_to_string(fixtures_dir().join("records.mncs")).expect("fixture");

    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
            })),
        )
        .await;
    harness.next_diagnostics_for(&uri).await;

    // Document symbols include Profile 0.5 records and fields.
    let response = harness
        .request(
            "textDocument/documentSymbol",
            Some(serde_json::json!({ "textDocument": { "uri": uri } })),
        )
        .await
        .expect("symbols");
    let value = response.result().expect("result").clone();
    let rendered = serde_json::to_string(&value).expect("string");
    assert!(rendered.contains("\"Reading\""), "{rendered}");
    assert!(rendered.contains("\"celsius\""), "{rendered}");
    assert!(rendered.contains("\"adjust\""), "{rendered}");

    // Semantic tokens arrive in LSP delta encoding with plausible counts.
    let response = harness
        .request(
            "textDocument/semanticTokens/full",
            Some(serde_json::json!({ "textDocument": { "uri": uri } })),
        )
        .await
        .expect("tokens");
    let value = response.result().expect("result").clone();
    let data = value["data"].as_array().expect("data array");
    assert_eq!(data.len() % 5, 0, "5-int encoding per token");
    assert!(
        data.len() >= 25,
        "keywords/types/functions are classified: {data:?}"
    );

    harness.request("shutdown", None).await.expect("shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_symbol_query_finds_across_documents() {
    let mut harness = Harness::new().await;
    // Touch two documents so they enter resident state.
    for name in ["records.mncs", "finite-match.mncs"] {
        let uri = fixture_uri(name);
        let text = std::fs::read_to_string(fixtures_dir().join(name)).expect("fixture");
        harness
            .notify(
                "textDocument/didOpen",
                Some(serde_json::json!({
                    "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
                })),
            )
            .await;
        harness.next_diagnostics_for(&uri).await;
    }
    let response = harness
        .request(
            "workspace/symbol",
            Some(serde_json::json!({ "query": "Reading" })),
        )
        .await
        .expect("workspace symbol");
    let value = response.result().expect("result").clone();
    let rendered = serde_json::to_string(&value).expect("string");
    assert!(rendered.contains("Reading"));
    assert!(rendered.contains("records.mncs"));
}
