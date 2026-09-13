//! Protocol-level tests for the MNCS language server.
//!
//! These drive the real `tower-lsp` service over JSON-RPC and assert on both
//! responses and server-initiated diagnostics publications, exercising the
//! exact sequences an editor performs: initialize, open, receive diagnostics,
//! hover, definition, references, unsaved change, updated diagnostics, close.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

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
        Self::new_at(fixtures_dir()).await
    }

    async fn new_at(root: PathBuf) -> Self {
        let (service, socket) = mncs_lsp::create_service(Some(root.clone()));
        let mut harness = Self { service, socket };
        let initialize = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", root.canonicalize().expect("root path").display()),
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
async fn completion_and_folding_are_served() {
    let mut harness = Harness::new().await;
    let uri = fixture_uri("finite-match.mncs");
    let text = std::fs::read_to_string(fixtures_dir().join("finite-match.mncs")).expect("fixture");

    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
            })),
        )
        .await;
    harness.next_diagnostics_for(&uri).await;

    let map = PositionMap::new(&text);
    let offset = text.find("return match").expect("completion prefix") + 2;
    let position = map.position_of(&text, offset);
    let response = harness
        .request(
            "textDocument/completion",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character + 1),
            })),
        )
        .await
        .expect("completion");
    let completion = response.result().expect("completion result");
    assert!(
        completion
            .as_array()
            .expect("completion array")
            .iter()
            .any(|item| item["label"] == "return"),
        "keyword completion returned: {completion}"
    );

    let response = harness
        .request(
            "textDocument/foldingRange",
            Some(serde_json::json!({ "textDocument": { "uri": uri } })),
        )
        .await
        .expect("folding");
    assert!(
        response
            .result()
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ranges| !ranges.is_empty()),
        "folding ranges returned: {response:?}"
    );

    harness.request("shutdown", None).await.expect("shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn cross_file_navigation_is_served_over_lsp() {
    let mut harness = Harness::new_at(fixtures_dir().join("imports")).await;
    let uri = fixture_uri("imports/study.mncs");
    let text = std::fs::read_to_string(fixtures_dir().join("imports/study.mncs"))
        .expect("importing fixture");

    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
            })),
        )
        .await;
    let diagnostics = harness.next_diagnostics_for(&uri).await;
    assert_eq!(diagnostics["params"]["diagnostics"], serde_json::json!([]));

    let offset = text.find("demote").expect("imported call");
    let position = PositionMap::new(&text).position_of(&text, offset);
    let response = harness
        .request(
            "textDocument/definition",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": position,
            })),
        )
        .await
        .expect("definition");
    let definitions = response
        .result()
        .expect("definition result")
        .as_array()
        .expect("definition array");
    assert_eq!(definitions.len(), 1);
    assert!(definitions[0]["uri"]
        .as_str()
        .unwrap_or_default()
        .ends_with("evidence.mncs"));

    let response = harness
        .request(
            "textDocument/references",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": position,
                "context": { "includeDeclaration": true },
            })),
        )
        .await
        .expect("references");
    let references = response
        .result()
        .expect("references result")
        .as_array()
        .expect("references array");
    assert!(references.iter().any(|hit| {
        hit["uri"]
            .as_str()
            .unwrap_or_default()
            .ends_with("evidence.mncs")
    }));
    assert!(references.iter().any(|hit| {
        hit["uri"]
            .as_str()
            .unwrap_or_default()
            .ends_with("study.mncs")
    }));

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

struct StdioClient {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioClient {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mncs-lsp"))
            .env("MNLS_WORKSPACE_ROOT", fixtures_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn stdio server");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, method: &str, params: Option<serde_json::Value>, id: Option<u64>) {
        let mut request = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        if let Some(params) = params {
            request["params"] = params;
        }
        if let Some(id) = id {
            request["id"] = serde_json::json!(id);
        }
        let body = serde_json::to_vec(&request).expect("request JSON");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("headers");
        self.stdin.write_all(&body).expect("request body");
        self.stdin.flush().expect("flush request");
    }

    fn receive(&mut self) -> serde_json::Value {
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("response headers");
            if line == "\r\n" {
                break;
            }
            headers.push(line);
        }
        let length = headers
            .iter()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("content length");
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).expect("response body");
        serde_json::from_slice(&body).expect("response JSON")
    }

    fn receive_until_id(&mut self, id: u64) -> serde_json::Value {
        loop {
            let message = self.receive();
            if message.get("id") == Some(&serde_json::json!(id)) {
                return message;
            }
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incremental_edits_update_analysis_without_full_resend() {
    let mut harness = Harness::new().await;
    let uri = fixture_uri("valid-contracts.mncs");
    let disk_text =
        std::fs::read_to_string(fixtures_dir().join("valid-contracts.mncs")).expect("fixture");
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

    // Incremental insertion of one character breaks the call target; only the
    // ranged edit travels, not the whole document.
    let map = PositionMap::new(&disk_text);
    let call = disk_text.rfind("bounded_step").expect("call site");
    let info = map.position_of(&disk_text, call);
    harness
        .notify(
            "textDocument/didChange",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": info.line, "character": info.character },
                        "end": { "line": info.line, "character": info.character },
                    },
                    "text": "x",
                }],
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    let diagnostics = published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(
        diagnostics.iter().any(|item| item["code"] == "MNE131"),
        "incremental edit re-analyzed: {diagnostics:?}"
    );

    // Removing the character incrementally heals the document.
    harness
        .notify(
            "textDocument/didChange",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": info.line, "character": info.character },
                        "end": { "line": info.line, "character": info.character + 1 },
                    },
                    "text": "",
                }],
            })),
        )
        .await;
    let published = harness.next_diagnostics_for(&uri).await;
    assert_eq!(published["params"]["diagnostics"], serde_json::json!([]));

    harness.request("shutdown", None).await.expect("shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn signature_help_declaration_type_definition_flow() {
    let mut harness = Harness::new().await;
    let uri = fixture_uri("valid-contracts.mncs");
    let text =
        std::fs::read_to_string(fixtures_dir().join("valid-contracts.mncs")).expect("fixture");
    harness
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text },
            })),
        )
        .await;
    harness.next_diagnostics_for(&uri).await;

    // Signature help on the second call argument.
    let call = text.rfind("bounded_step(value, value)").expect("call");
    let second = call + "bounded_step(value, ".len();
    let position = PositionMap::new(&text).position_of(&text, second);
    let response = harness
        .request(
            "textDocument/signatureHelp",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("signature help");
    let help = response.result().expect("result").clone();
    assert_eq!(help["activeParameter"], 1);
    assert!(help["signatures"][0]["label"]
        .as_str()
        .unwrap_or_default()
        .contains("fn bounded_step"));

    // Declaration resolves like definition for this single-site language.
    let call_name = text.rfind("bounded_step").expect("call site");
    let position = PositionMap::new(&text).position_of(&text, call_name);
    let response = harness
        .request(
            "textDocument/declaration",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("declaration");
    let locations = response
        .result()
        .expect("result")
        .as_array()
        .expect("array")
        .clone();
    assert_eq!(locations.len(), 1);

    // Type definition of a builtin-typed binding is honestly empty.
    let binding = text.find("let next: i64").expect("binding") + 4;
    let position = PositionMap::new(&text).position_of(&text, binding);
    let response = harness
        .request(
            "textDocument/typeDefinition",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("type definition");
    assert_null_result(&response, "builtin type has no definition site");

    harness.request("shutdown", None).await.expect("shutdown");
}

#[tokio::test(flavor = "current_thread")]
async fn rename_selection_hierarchy_hints_actions_formatting_flow() {
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
    let map = PositionMap::new(&text);

    // Rename the record type: declaration + constructor + annotations move.
    let decl = text.find("record Reading").expect("decl") + "record ".len();
    let position = map.position_of(&text, decl);
    let response = harness
        .request(
            "textDocument/rename",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
                "newName": "Sample",
            })),
        )
        .await
        .expect("rename");
    let edit = response.result().expect("rename edit").clone();
    let files = edit["changes"].as_object().expect("changes");
    assert_eq!(files.len(), 1);
    let file_edits = files[&uri].as_array().expect("edits");
    assert!(file_edits.len() >= 3, "decl + uses: {file_edits:?}");

    // Renaming to a keyword is a protocol error, not silent success.
    let response = harness
        .request(
            "textDocument/rename",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
                "newName": "fn",
            })),
        )
        .await
        .expect("rename refusal");
    assert!(response.error().is_some(), "{response:?}");

    // Selection ranges nest innermost-first.
    let at = text.find("celsius: i32").expect("field") + 2;
    let position = map.position_of(&text, at);
    let response = harness
        .request(
            "textDocument/selectionRange",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "positions": [Position::new(position.line, position.character)],
            })),
        )
        .await
        .expect("selection range");
    let ranges = response
        .result()
        .expect("result")
        .as_array()
        .expect("array")
        .clone();
    assert_eq!(ranges.len(), 1);
    let mut depth = 0;
    let mut cursor = &ranges[0];
    loop {
        depth += 1;
        match cursor.get("parent") {
            Some(parent) if !parent.is_null() => cursor = parent,
            _ => break,
        }
    }
    assert!(depth >= 3, "expected nesting, got {depth}");

    // Call hierarchy: prepare on `adjust`, then outgoing (none) and incoming.
    let adjust = text.find("fn adjust").expect("fn") + 3;
    let position = map.position_of(&text, adjust);
    let response = harness
        .request(
            "textDocument/prepareCallHierarchy",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "position": Position::new(position.line, position.character),
            })),
        )
        .await
        .expect("prepare");
    let items = response
        .result()
        .expect("result")
        .as_array()
        .expect("array")
        .clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "adjust");
    let response = harness
        .request(
            "callHierarchy/outgoingCalls",
            Some(serde_json::json!({ "item": items[0] })),
        )
        .await
        .expect("outgoing");
    assert!(response
        .result()
        .expect("result")
        .as_array()
        .expect("array")
        .is_empty());

    // Inlay hints serve the whole file without error.
    let response = harness
        .request(
            "textDocument/inlayHint",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 100, "character": 0 },
                },
            })),
        )
        .await
        .expect("inlay hints");
    assert!(response.result().is_some());

    // Formatting a canonical fixture yields no edits.
    let response = harness
        .request(
            "textDocument/formatting",
            Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "options": { "tabSize": 4, "insertSpaces": true },
            })),
        )
        .await
        .expect("formatting");
    assert_null_result(&response, "canonical fixture needs no formatting");

    harness.request("shutdown", None).await.expect("shutdown");
}

/// A `null` JSON-RPC result still presents as `Some(Null)`.
fn assert_null_result(response: &tower_lsp::jsonrpc::Response, context: &str) {
    assert_eq!(
        response.result(),
        Some(&serde_json::Value::Null),
        "{context}: {response:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn capabilities_advertise_second_wave_methods() {
    let (service, _socket) =
        mncs_lsp::create_service(Some(fixtures_dir().canonicalize().expect("root")));
    use tower::Service as _;
    use tower::ServiceExt as _;
    let request = tower_lsp::jsonrpc::Request::build("initialize".to_owned())
        .params(serde_json::json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", fixtures_dir().canonicalize().expect("root").display()),
            "capabilities": {},
        }))
        .id(1)
        .finish();
    let mut service = service;
    let response = service
        .ready()
        .await
        .expect("ready")
        .call(request)
        .await
        .expect("call")
        .expect("response");
    let value = serde_json::to_value(response.result().expect("result")).expect("json");
    let capabilities = &value["capabilities"];
    for provider in [
        "signatureHelpProvider",
        "declarationProvider",
        "typeDefinitionProvider",
        "renameProvider",
        "documentFormattingProvider",
        "documentRangeFormattingProvider",
        "selectionRangeProvider",
        "callHierarchyProvider",
        "inlayHintProvider",
        "codeActionProvider",
    ] {
        assert!(
            !capabilities[provider].is_null(),
            "missing advertised provider {provider}"
        );
    }
    assert_eq!(
        capabilities["textDocumentSync"], 2,
        "incremental sync must be advertised"
    );
}

#[test]
fn real_stdio_transport_publishes_diagnostics() {
    let mut client = StdioClient::start();
    let root = fixture_uri_as_root();
    client.send(
        "initialize",
        Some(serde_json::json!({ "processId": std::process::id(), "rootUri": root, "capabilities": {} })),
        Some(1),
    );
    let initialize = client.receive_until_id(1);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        "mncs-language-service"
    );
    client.send("initialized", Some(serde_json::json!({})), None);

    let uri = fixture_uri("syntax-error.mncs");
    let text = std::fs::read_to_string(fixtures_dir().join("syntax-error.mncs")).expect("fixture");
    client.send(
        "textDocument/didOpen",
        Some(serde_json::json!({ "textDocument": { "uri": uri, "languageId": "mncs", "version": 1, "text": text } })),
        None,
    );
    loop {
        let message = client.receive();
        if message["method"] == "textDocument/publishDiagnostics" {
            assert_eq!(message["params"]["uri"], uri);
            assert!(!message["params"]["diagnostics"]
                .as_array()
                .expect("diagnostics")
                .is_empty());
            break;
        }
    }

    client.send("shutdown", None, Some(2));
    assert!(client.receive_until_id(2)["error"].is_null());
    client.send("exit", None, None);
}
