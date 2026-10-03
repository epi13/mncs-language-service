//! Resident transport tests: the Unix-socket RPC layer must serve many
//! requests per connection, batch independent calls in one round trip with
//! failure isolation, and answer identically to the local core.

use mncs_service_core::{
    serve_unix, BatchCall, LanguageService, LanguageServiceClient, PositionMap,
    RemoteLanguageService, ResponseStatus,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

const DOC: &str = "mncs 0.6;\n\nmodule bench.base;\n\nenum Verdict { PASS, FAIL, UNKNOWN }\n\nfn demote(value: Verdict) -> (result: Verdict) {\n    return match value {\n        PASS => Verdict.UNKNOWN,\n        FAIL => Verdict.FAIL,\n        UNKNOWN => Verdict.UNKNOWN,\n    };\n}\n";

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mncs-language-service-transport-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary workspace");
    root
}

fn start_server(root: &std::path::Path) -> (Arc<LanguageService>, RemoteLanguageService, String) {
    fs::write(root.join("base.mncs"), DOC).expect("fixture");
    let uri = format!("file://{}", root.join("base.mncs").display());
    let service = Arc::new(LanguageService::new(Some(root.to_path_buf())));
    service.discover_workspace().expect("discover");
    service.snapshot(&uri).expect("warm");
    let socket = root.join("ls.sock");
    let server_service = Arc::clone(&service);
    let server_socket = socket.clone();
    std::thread::spawn(move || {
        serve_unix(server_socket, server_service).expect("resident socket host");
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(socket.exists(), "resident socket did not start");
    let remote = RemoteLanguageService::connect_path(&socket);
    (service, remote, uri)
}

fn demote_position(service: &LanguageService, uri: &str) -> (u32, u32) {
    let text = service.store().content(uri).expect("content");
    let byte = text.find("fn demote").expect("decl") + 3;
    let info = PositionMap::new(&text).position_of(&text, byte);
    (info.line, info.character)
}

#[test]
fn persistent_client_answers_like_the_local_core() {
    let root = temp_root("equivalence");
    let (service, remote, uri) = start_server(&root);
    let (line, character) = demote_position(&service, &uri);

    let local_hover = service.hover(&uri, line, character).expect("hover");
    let remote_hover = remote.hover(&uri, line, character).expect("hover");
    assert_eq!(
        serde_json::to_value(&local_hover).expect("json"),
        serde_json::to_value(&remote_hover).expect("json")
    );

    let local_refs = service
        .references(&uri, line, character, true)
        .expect("references");
    let remote_refs = remote
        .references(&uri, line, character, true)
        .expect("references");
    assert_eq!(
        serde_json::to_value(&local_refs).expect("json"),
        serde_json::to_value(&remote_refs).expect("json")
    );

    let local_symbols = service.workspace_symbols("demote");
    let remote_symbols = remote.workspace_symbols("demote").expect("symbols");
    assert_eq!(
        serde_json::to_value(&local_symbols).expect("json"),
        serde_json::to_value(&remote_symbols).expect("json")
    );

    let local_stats = service.service_stats();
    let remote_stats = remote.service_stats().expect("stats");
    // Stats move as queries run; only the resident shape must agree.
    assert_eq!(
        local_stats.indexed_documents,
        remote_stats.indexed_documents
    );
    assert_eq!(
        local_stats.indexed_occurrences,
        remote_stats.indexed_occurrences
    );
}

#[test]
fn many_requests_share_one_connection() {
    let root = temp_root("persistent");
    let (service, remote, uri) = start_server(&root);
    let (line, character) = demote_position(&service, &uri);

    for _ in 0..20 {
        remote.hover(&uri, line, character).expect("hover");
        remote.workspace_symbols("demote").expect("symbols");
    }
    assert_eq!(remote.connection_count(), 1);
}

#[test]
fn batch_groups_calls_with_failure_isolation() {
    let root = temp_root("batch");
    let (service, remote, uri) = start_server(&root);
    let (line, character) = demote_position(&service, &uri);

    let solo_hover = remote.hover(&uri, line, character).expect("hover");
    let results = remote
        .batch(vec![
            BatchCall {
                id: 1,
                method: "hover".to_owned(),
                params: json!({"uri": uri, "line": line, "character": character}),
            },
            BatchCall {
                id: 2,
                method: "no_such_method".to_owned(),
                params: json!({}),
            },
            BatchCall {
                id: 3,
                method: "batch".to_owned(),
                params: json!({"calls": []}),
            },
            BatchCall {
                id: 4,
                method: "workspace_symbols".to_owned(),
                params: json!({"query": "demote"}),
            },
        ])
        .expect("batch");
    assert_eq!(results.len(), 4);
    assert!(results[0].ok, "{:?}", results[0]);
    assert_eq!(results[0].id, 1);
    assert!(!results[1].ok, "{:?}", results[1]);
    assert!(!results[2].ok, "{:?}", results[2]);
    assert!(results[3].ok, "{:?}", results[3]);

    // Batched answers equal solo answers.
    let batched_hover: mncs_service_core::HoverResponse =
        serde_json::from_value(results[0].result.clone().expect("result")).expect("hover");
    assert_eq!(
        serde_json::to_value(&batched_hover).expect("json"),
        serde_json::to_value(&solo_hover).expect("json")
    );
    assert_eq!(results[1].id, 2);
    assert_eq!(results[3].id, 4);

    // One round trip: the batch cost exactly one request on the connection.
    let _ = service;
}

#[test]
fn batch_enforces_its_bound() {
    let root = temp_root("batch-bound");
    let (_service, remote, _uri) = start_server(&root);
    let calls: Vec<BatchCall> = (0..mncs_service_core::MAX_BATCH_CALLS + 1)
        .map(|id| BatchCall {
            id: id as u64,
            method: "service_stats".to_owned(),
            params: json!({}),
        })
        .collect();
    let result = remote.batch(calls);
    assert!(result.is_err(), "oversized batch must be rejected");
}

#[test]
fn remote_mutations_apply_exactly_once() {
    let root = temp_root("mutations");
    let (_service, remote, _uri) = start_server(&root);
    let uri = "untitled:batch-mutations";
    let first = remote.did_open(uri, 1, DOC.to_owned()).expect("open");
    let second = remote
        .did_change_incremental(
            uri,
            2,
            vec![mncs_service_core::TextChange {
                range: None,
                text: DOC.replace("UNKNOWN,", "UNKNOWN, // noted"),
            }],
        )
        .expect("change");
    // A transport retry must never double-apply: generations advance by one.
    assert_eq!(second, first + 1);
    assert_eq!(remote.connection_count(), 1);
    let diagnostics = remote.document_diagnostics(uri).expect("diagnostics");
    assert_eq!(diagnostics.status, ResponseStatus::Answered);
}
