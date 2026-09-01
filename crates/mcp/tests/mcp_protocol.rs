//! Protocol-level tests for the MCP adapter.
//!
//! These run the real `rmcp` server over an in-memory duplex transport and
//! drive it with a real MCP client, exercising the same tool surface agents
//! use — against the same resident core the LSP adapter serves.

use std::path::PathBuf;
use std::sync::Arc;

use mncs_service_core::LanguageService;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ServiceExt as _;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

struct ClientHarness {
    /// Held to keep the MCP session alive.
    _client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    /// Held so the server task is never abandoned mid-session.
    _server_task: tokio::task::JoinHandle<()>,
}

async fn spawn_server() -> ClientHarness {
    let service = Arc::new(LanguageService::new(Some(fixtures_dir())));
    let mut server = mncs_mcp::MncsSemanticServer::new(service);
    server
        .set_root(Some(fixtures_dir()))
        .expect("workspace root");
    let (client_transport, server_transport) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        match server.serve(server_transport).await {
            Ok(running) => {
                eprintln!("MCP SERVE STARTED");
                match running.waiting().await {
                    Ok(_) => eprintln!("MCP WAITING DONE clean"),
                    Err(error) => eprintln!("MCP WAITING ERR {error:?}"),
                }
            }
            Err(error) => eprintln!("MCP SERVE ERR {error:?}"),
        }
    });
    let client = ().serve(client_transport).await.expect("client initialization");
    let peer = client.peer().clone();
    ClientHarness {
        _client: client,
        peer,
        _server_task: server_task,
    }
}

fn uri_for(name: &str) -> String {
    format!(
        "file://{}",
        fixtures_dir()
            .join(name)
            .canonicalize()
            .expect("fixture path")
            .display()
    )
}

async fn call(
    peer: &rmcp::service::Peer<rmcp::RoleClient>,
    name: String,
    arguments: serde_json::Value,
) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name.clone());
    params.arguments = Some(arguments.as_object().cloned().unwrap_or_default());
    peer.call_tool(params).await.expect("tool call succeeds")
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_expected_read_only_tools() {
    let harness = spawn_server().await;
    let peer = &harness.peer;
    let tools = peer.list_tools(None).await.expect("list tools");
    let names: Vec<&str> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in [
        "workspace_status",
        "document_diagnostics",
        "identity_at_position",
        "describe_subject",
        "find_definition",
        "find_references",
        "list_symbols",
        "semantic_dependencies",
        "obligations",
        "native_obligations",
        "context_packet",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }
    // Every tool declares read-only intent.
    for tool in &tools.tools {
        if let Some(annotations) = &tool.annotations {
            assert!(
                annotations.read_only_hint.unwrap_or(false),
                "{} must be read-only",
                tool.name
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_and_diagnostics_tools_answer() {
    let harness = spawn_server().await;
    let peer = &harness.peer;

    let status = call(peer, "workspace_status".to_owned(), serde_json::json!({})).await;
    assert_ne!(status.is_error, Some(true));
    let structured = status.structured_content.expect("structured");
    assert!(structured["documents"].as_array().expect("documents").len() >= 6);

    let broken_uri = uri_for("syntax-error.mncs");
    let diagnostics = call(
        peer,
        "document_diagnostics".to_owned(),
        serde_json::json!({ "uri": broken_uri }),
    )
    .await;
    let payload = diagnostics.structured_content.expect("structured");
    let codes: Vec<&str> = payload["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["code"].as_str().expect("code"))
        .collect();
    assert_eq!(codes, vec!["MNP016"]);
    assert_eq!(payload["snapshot"]["language_profile"], "0.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_describe_and_dependencies_agree() {
    let harness = spawn_server().await;
    let peer = &harness.peer;
    let uri = uri_for("valid-contracts.mncs");

    // Identity at the declaration of bounded_step.
    let text = std::fs::read_to_string(fixtures_dir().join("valid-contracts.mncs")).expect("text");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn bounded_step").expect("needle") + 3;
    let position = map.position_of(&text, offset);

    let identity_response = call(
        peer,
        "identity_at_position".to_owned(),
        serde_json::json!({
            "uri": uri,
            "line": position.line,
            "character": position.character,
        }),
    )
    .await;
    let payload = identity_response.structured_content.expect("structured");
    let occurrences = payload["occurrences"].as_array().expect("occurrences");
    assert_eq!(occurrences.len(), 1);
    let identity = occurrences[0]["symbol"]["identity"]
        .as_str()
        .expect("identity");
    assert_eq!(
        identity,
        "mncs:0.2:function:examples.contracts::bounded_step"
    );

    // Describe the same subject by identity.
    let described = call(
        peer,
        "describe_subject".to_owned(),
        serde_json::json!({ "uri": uri, "identity": identity }),
    )
    .await;
    let subject = described.structured_content.expect("structured")["subject"].clone();
    assert_eq!(subject["summary"]["name"], "bounded_step");

    // Dependents via the calls graph.
    let dependents = call(
        peer,
        "semantic_dependencies".to_owned(),
        serde_json::json!({ "uri": uri, "identity": identity, "direction": "incoming" }),
    )
    .await;
    let incoming = dependents.structured_content.expect("structured")["incoming"]
        .as_array()
        .expect("incoming")
        .clone();
    assert!(
        incoming.iter().any(|edge| edge["name"] == "caller"),
        "{incoming:?}"
    );

    // Obligations preserve UNKNOWN for unevidenced contracts.
    let obligations = call(
        peer,
        "obligations".to_owned(),
        serde_json::json!({ "uri": uri }),
    )
    .await;
    let payload = obligations.structured_content.expect("structured");
    assert!(payload["counts"]["unknown"].as_u64().expect("count") > 0);

    // Context packet around the CALLER is honest about completeness under a
    // one-excerpt budget: its callee cannot fit.
    let map2 = mncs_service_core::PositionMap::new(&text);
    let caller_offset = text.find("fn caller").expect("caller") + 3;
    let caller_position = map2.position_of(&text, caller_offset);
    let caller_response = call(
        peer,
        "identity_at_position".to_owned(),
        serde_json::json!({
            "uri": uri,
            "line": caller_position.line,
            "character": caller_position.character,
        }),
    )
    .await;
    let caller_identity = caller_response.structured_content.expect("structured")["occurrences"][0]
        ["symbol"]["identity"]
        .as_str()
        .expect("caller identity")
        .to_owned();

    let packet = call(
        peer,
        "context_packet".to_owned(),
        serde_json::json!({ "uri": uri, "identity": caller_identity, "max_excerpts": 1 }),
    )
    .await;
    let payload = packet.structured_content.expect("structured");
    assert_eq!(payload["complete"], false);
    assert!(payload["notes"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn native_obligations_executes_through_the_real_tool_surface() {
    let library = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-language/library");
    std::env::set_var("MNCS_LIBRARY_PATH", library);

    let harness = spawn_server().await;
    let result = call(
        &harness.peer,
        "native_obligations".to_owned(),
        serde_json::json!({ "uri": uri_for("valid-contracts.mncs") }),
    )
    .await;
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
    let payload = result.structured_content.expect("structured content");
    assert_eq!(payload["status"]["kind"], "answered");
    assert_eq!(payload["counts"], payload["reference_counts"]);
    assert_eq!(payload["native"]["backend"], "mncs-research-bytecode");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_documents_fail_without_crashing_the_server() {
    let harness = spawn_server().await;
    let peer = &harness.peer;
    let result = call(
        peer,
        "document_diagnostics".to_owned(),
        serde_json::json!({ "uri": "file:///nowhere/missing.mncs" }),
    )
    .await;
    assert_eq!(result.is_error, Some(true));

    // The server still answers afterwards.
    let status = call(peer, "workspace_status".to_owned(), serde_json::json!({})).await;
    assert_ne!(status.is_error, Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn analyze_candidate_returns_deltas_without_mutating_the_workspace() {
    let harness = spawn_server().await;
    let peer = &harness.peer;
    let uri = uri_for("records.mncs");
    let baseline_text =
        std::fs::read_to_string(fixtures_dir().join("records.mncs")).expect("fixture text");
    let candidate_text =
        baseline_text.replace("return updated.celsius;", "return updated.celsius + 1;");
    assert_ne!(candidate_text, baseline_text);

    let result = call(
        peer,
        "analyze_candidate".to_owned(),
        serde_json::json!({ "uri": uri, "candidate_text": candidate_text }),
    )
    .await;
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
    let raw = result
        .structured_content
        .expect("structured content")
        .to_string();
    assert!(raw.contains("baselineSourceIdentity") || raw.contains("baseline_source_identity"));
    assert!(raw.contains("stale_evidence") || raw.contains("staleEvidence"));

    // The workspace still reports the untouched baseline document.
    let status = call(peer, "workspace_status".to_owned(), serde_json::json!({})).await;
    let text = status.content[0].as_text().expect("text").text.clone();
    assert!(
        !text.contains("candidate"),
        "workspace status must not adopt candidate state"
    );
}
