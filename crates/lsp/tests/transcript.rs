//! Deterministic LSP transcript test.
//!
//! A fixed input sequence (initialize, open, hover, completion, definition,
//! incremental edit, diagnostics, shutdown) produces a canonical transcript
//! compared against `golden/transcript.json`. Absolute fixture paths are
//! redacted to `$FIXTURES`; content hashes are content-derived and stay.
//!
//! To intentionally update after a reviewed semantic change:
//! `UPDATE_GOLDEN=1 cargo test -p mncs-lsp --test transcript`. Never update
//! blindly: inspect the diff first.

use std::path::PathBuf;

use futures::StreamExt;
use mncs_service_core::PositionMap;
use tower::Service as _;
use tower::ServiceExt as _;
use tower_lsp::jsonrpc::{Request, Response as RpcResponse};
use tower_lsp::lsp_types::Position;
use tower_lsp::{ClientSocket, LspService};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/transcript.json")
}

struct Harness {
    service: LspService<mncs_lsp::Backend>,
    socket: ClientSocket,
}

impl Harness {
    async fn new_at(root: PathBuf) -> Self {
        let (service, socket) = mncs_lsp::create_service(Some(root.clone()));
        let mut harness = Self { service, socket };
        let initialize = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", root.canonicalize().expect("root path").display()),
            "capabilities": {},
        });
        harness.request("initialize", Some(initialize)).await;
        harness
            .notify("initialized", Some(serde_json::json!({})))
            .await;
        harness.drain_socket().await;
        harness
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<RpcResponse> {
        let request = match params {
            Some(params) => Request::build(method.to_owned())
                .params(params)
                .id(1)
                .finish(),
            None => Request::build(method.to_owned()).id(1).finish(),
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
        self.service
            .ready()
            .await
            .expect("service ready")
            .call(builder.finish())
            .await
            .expect("notification accepted");
    }

    async fn drain_socket(&mut self) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();
        while let Ok(Some(message)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), self.socket.next()).await
        {
            messages.push(serde_json::to_value(&message).expect("serializable message"));
        }
        messages
    }

    async fn next_diagnostics_for(&mut self, uri: &str) -> serde_json::Value {
        for _ in 0..20 {
            if let Ok(Some(message)) =
                tokio::time::timeout(std::time::Duration::from_millis(500), self.socket.next())
                    .await
            {
                let value = serde_json::to_value(&message).expect("message");
                if value.get("method").and_then(|method| method.as_str())
                    == Some("textDocument/publishDiagnostics")
                    && value["params"]["uri"].as_str() == Some(uri)
                {
                    return value;
                }
            }
        }
        panic!("no diagnostics published for {uri}");
    }
}

/// Redact machine-specific paths; keep content hashes (they are the point).
fn redact(value: serde_json::Value, fixtures: &str) -> serde_json::Value {
    let mut text = serde_json::to_string(&value).expect("serializable");
    text = text.replace(fixtures, "$FIXTURES");
    serde_json::from_str(&text).expect("valid JSON")
}

#[tokio::test(flavor = "current_thread")]
async fn golden_transcript_matches_canonical_output() {
    let root = fixtures_dir();
    let fixtures = root.canonicalize().expect("root").display().to_string();
    let uri = format!(
        "file://{}/valid-contracts.mncs",
        fixtures_dir().canonicalize().expect("path").display()
    );
    let text =
        std::fs::read_to_string(fixtures_dir().join("valid-contracts.mncs")).expect("fixture");

    let mut steps: Vec<serde_json::Value> = Vec::new();
    let mut record = |step: &str, value: serde_json::Value| {
        steps.push(serde_json::json!({ "step": step, "output": redact(value, &fixtures) }));
    };

    // 1. Capabilities advertised at initialize (dedicated service: the
    // interaction harness below already initializes once).
    let capabilities = {
        let (mut service, _socket) = mncs_lsp::create_service(Some(root.clone()));
        let request = Request::build("initialize".to_owned())
            .params(serde_json::json!({
                "processId": 1,
                "rootUri": format!("file://{fixtures}"),
                "capabilities": {},
            }))
            .id(1)
            .finish();
        service
            .ready()
            .await
            .expect("ready")
            .call(request)
            .await
            .expect("call")
            .expect("response")
            .result()
            .expect("result")
            .clone()
    };
    record(
        "initialize",
        serde_json::json!({
            "textDocumentSync": capabilities["capabilities"]["textDocumentSync"],
            "hoverProvider": capabilities["capabilities"]["hoverProvider"],
            "definitionProvider": capabilities["capabilities"]["definitionProvider"],
            "renameProvider": capabilities["capabilities"]["renameProvider"],
            "documentFormattingProvider": capabilities["capabilities"]["documentFormattingProvider"],
            "callHierarchyProvider": capabilities["capabilities"]["callHierarchyProvider"],
            "inlayHintProvider": capabilities["capabilities"]["inlayHintProvider"],
            "codeActionProvider": capabilities["capabilities"]["codeActionProvider"],
            "signatureHelpProvider": capabilities["capabilities"]["signatureHelpProvider"],
        }),
    );

    let mut harness = Harness::new_at(root).await;

    // 2. Open → clean diagnostics.
    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    record(
        "didOpen.diagnostics",
        published["params"]["diagnostics"].clone(),
    );

    // 3. Hover over the declaration.
    let map = PositionMap::new(&text);
    let decl = text.find("fn bounded_step").expect("decl") + 3;
    let position = map.position_of(&text, decl);
    let response = harness
        .request(
            "textDocument/hover",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("hover");
    record("hover", response.result().expect("result").clone());

    // 4. Completion labels at the call site (labels only: stable vocabulary).
    let call = text.rfind("bounded_step").expect("call");
    let position = map.position_of(&text, call);
    let response = harness
        .request(
            "textDocument/completion",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("completion");
    let labels = response.result().expect("result").clone();
    let labels = labels
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item["label"].clone())
        .collect::<Vec<_>>();
    record("completion.labels", serde_json::json!(labels));

    // 5. Go to definition from the call site.
    let response = harness
        .request(
            "textDocument/definition",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("definition");
    record("definition", response.result().expect("result").clone());

    // 6. Incremental one-character edit → diagnostics with codes.
    harness
        .notify(
            "textDocument/didChange",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": position.line, "character": position.character },
                        "end": { "line": position.line, "character": position.character },
                    },
                    "text": "x",
                }],
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    let codes = published["params"]["diagnostics"]
        .as_array()
        .expect("array")
        .iter()
        .map(|item| {
            serde_json::json!({
                "code": item["code"],
                "message": item["message"],
                "range": item["range"],
            })
        })
        .collect::<Vec<_>>();
    record("didChange.diagnostics", serde_json::json!(codes));

    // 7. Signature help still answers on the broken buffer: the declaration
    // signature is intact even though the call site no longer resolves.
    let param = text.find("limit: i64").expect("param") + 2;
    let sig_position = map.position_of(&text, param);
    let response = harness
        .request(
            "textDocument/signatureHelp",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(sig_position.line, sig_position.character),
            })),
        )
        .await
        .expect("signature help");
    record(
        "signatureHelp.declaration",
        response
            .result()
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );

    harness.request("shutdown", None).await.expect("shutdown");

    let transcript = serde_json::json!({ "version": 1, "steps": steps });
    let pretty = serde_json::to_string_pretty(&transcript).expect("pretty") + "\n";
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("golden dir");
        std::fs::write(&path, &pretty).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden file {}; run with UPDATE_GOLDEN=1 to create it after review",
            path.display()
        )
    });
    if expected != pretty {
        panic!(
            "transcript drift: rerun with UPDATE_GOLDEN=1 only after reviewing the diff.\n--- expected ---\n{expected}\n--- actual ---\n{pretty}"
        );
    }
}
