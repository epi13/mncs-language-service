//! Ambient semantic-coherence tests: resident status identity, bounded
//! capsules, worktree isolation, restart reconciliation, and cursor
//! correctness through the same core the socket host serves.

use mncs_service_core::{
    serve_unix, LanguageService, LanguageServiceClient, RemoteLanguageService, ResponseStatus,
    CAPSULE_ENVELOPE_CAPACITY, SEMANTIC_CAPSULE_SCHEMA_VERSION, SERVICE_STATUS_SCHEMA_VERSION,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn temp_workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mncs-language-service-ambient-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary workspace");
    root
}

fn seed_workspace(root: &std::path::Path, names: &[&str]) {
    for name in names {
        fs::copy(fixtures_dir().join(name), root.join(name)).expect("fixture copy");
    }
}

fn workspace_uri(root: &std::path::Path, name: &str) -> String {
    format!("file://{}", root.join(name).display())
}

#[test]
fn service_status_binds_service_workspace_and_toolchain_identity() {
    let root = temp_workspace("status");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = LanguageService::new(None);
    service
        .configure_root(Some(root.clone()))
        .expect("configure root");

    let status = service.service_status().expect("service status");
    assert_eq!(status.schema_version, SERVICE_STATUS_SCHEMA_VERSION);
    assert_eq!(status.service.name, "mnls-language-service");
    assert!(!status.service.instance_id.is_empty());
    assert_eq!(status.service.pid, std::process::id());
    assert!(status.service.build_fingerprint.starts_with("sha256:"));
    let canonical = root.canonicalize().expect("canonical root");
    assert_eq!(
        status.workspace_root.as_deref(),
        Some(canonical.display().to_string().as_str())
    );
    assert!(!status.stream_identity.is_empty());
    assert_eq!(status.documents, 2);
    // The syntax-error fixture must surface as measured state, not prose.
    assert!(status.diagnostics.error > 0, "{status:#?}");
    assert!(status.readiness.ready, "{status:#?}");
    assert!(status.features.contains(&"semantic-capsule/1".to_owned()));
    assert!(status.toolchain_digest.starts_with("sha256:"));
    let checkpoint = status.checkpoint.expect("checkpoint observation");
    assert_eq!(checkpoint.stream_identity, status.stream_identity);
    // Toolchain-match is covered by the restart test, which owns the
    // process environment for its toolchain-change step; asserting it
    // here would race that controlled mutation across test threads.

    // A second process-equivalent service never shares the instance id.
    let other = LanguageService::new(None);
    other
        .configure_root(Some(root.clone()))
        .expect("second configure");
    let other_status = other.service_status().expect("second status");
    assert_ne!(other_status.service.instance_id, status.service.instance_id);
}

#[test]
fn separate_workspaces_never_share_semantic_state() {
    let first_root = temp_workspace("isolation-first");
    let second_root = temp_workspace("isolation-second");
    seed_workspace(&first_root, &["valid-contracts.mncs"]);
    seed_workspace(&second_root, &["syntax-error.mncs"]);

    let first = LanguageService::new(None);
    first
        .configure_root(Some(first_root.clone()))
        .expect("first root");
    let second = LanguageService::new(None);
    second
        .configure_root(Some(second_root.clone()))
        .expect("second root");

    let first_status = first.service_status().expect("first status");
    let second_status = second.service_status().expect("second status");
    assert_ne!(first_status.workspace_root, second_status.workspace_root);
    assert_ne!(first_status.stream_identity, second_status.stream_identity);
    assert_eq!(first_status.diagnostics.error, 0);
    assert!(second_status.diagnostics.error > 0);

    // An edit in one workspace advances only its own generation.
    let uri = workspace_uri(&first_root, "valid-contracts.mncs");
    let before = first_status.generation;
    first.did_save(&uri, None).expect("save");
    let after = first.service_status().expect("first status again");
    assert!(after.generation > before);
    let second_again = second.service_status().expect("second status again");
    assert_eq!(second_again.generation, second_status.generation);
    assert_eq!(
        second_again.diagnostics.error,
        second_status.diagnostics.error
    );
}

#[test]
fn capsule_is_bounded_admitted_by_policy_and_expandable() {
    let root = temp_workspace("capsule");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = LanguageService::new(None);
    service.configure_root(Some(root.clone())).expect("root");

    let capsule = service.semantic_capsule(None, 0).expect("fresh capsule");
    assert_eq!(capsule.schema_version, SEMANTIC_CAPSULE_SCHEMA_VERSION);
    assert_eq!(capsule.status, ResponseStatus::Answered, "{capsule:#?}");
    assert!(
        !capsule.window_matched,
        "fresh capsule has no resumed window"
    );
    assert!(capsule.measured.diagnostics > 0, "{capsule:#?}");
    let policy = capsule.policy.expect("policy evidence");
    assert!(policy.valid);
    assert!(policy.complete);
    assert_eq!(policy.backend, "mncs-research-bytecode");
    assert!(!policy.kernel_artifact_identity.is_empty());
    assert!(capsule.findings.len() <= CAPSULE_ENVELOPE_CAPACITY);
    assert_eq!(capsule.findings.len(), policy.admitted);

    // The syntax error is actionable and carries an exact expansion.
    let error = capsule
        .findings
        .iter()
        .find(|finding| finding.code.as_deref() == Some("MNP016"))
        .expect("syntax error admitted");
    assert_eq!(error.kind, "diagnostic");
    assert_eq!(error.relevance, "actionable");
    assert_eq!(error.expansion.method, "document_diagnostics");
    assert_eq!(
        error
            .expansion
            .params
            .get("uri")
            .and_then(|value| value.as_str()),
        error.uri.as_deref()
    );

    // Findings arrive in deterministic priority order.
    let ranks: Vec<(String, String)> = capsule
        .findings
        .iter()
        .map(|finding| (finding.relevance.clone(), finding.kind.clone()))
        .collect();
    let mut sorted = ranks.clone();
    sorted.sort_by(|left, right| {
        let relevance_rank = |relevance: &str| match relevance {
            "actionable" => 0,
            "watch" => 1,
            _ => 2,
        };
        relevance_rank(&left.0)
            .cmp(&relevance_rank(&right.0))
            .then(left.1.cmp(&right.1))
    });
    assert_eq!(ranks, sorted, "capsule order is deterministic");
}

#[test]
fn quiet_workspace_produces_an_empty_actionable_capsule() {
    let root = temp_workspace("quiet");
    seed_workspace(&root, &["records.mncs"]);
    let service = LanguageService::new(None);
    service.configure_root(Some(root.clone())).expect("root");

    let capsule = service.semantic_capsule(None, 0).expect("quiet capsule");
    assert_eq!(capsule.status, ResponseStatus::Answered, "{capsule:#?}");
    let policy = capsule.policy.as_ref().expect("policy evidence");
    assert_eq!(policy.actionable, 0, "{capsule:#?}");
    assert!(
        capsule
            .findings
            .iter()
            .all(|finding| finding.relevance != "actionable"),
        "{capsule:#?}"
    );
}

#[test]
fn capsule_window_resumes_or_reconciles_explicitly() {
    let root = temp_workspace("window");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let service = LanguageService::new(None);
    service.configure_root(Some(root.clone())).expect("root");
    let baseline = service.service_status().expect("baseline status");

    // A localized edit advances the stream.
    let uri = workspace_uri(&root, "valid-contracts.mncs");
    let original = fs::read_to_string(root.join("valid-contracts.mncs")).expect("read");
    service
        .did_save(
            &uri,
            Some(original.replace("return next;", "return next + 1;")),
        )
        .expect("edit");
    let live = service.service_status().expect("live status");
    assert!(live.event_cursor > baseline.event_cursor);

    // Resume with the correct stream identity: the window matches and the
    // changed subjects are measured.
    let resumed = service
        .semantic_capsule(Some(&live.stream_identity), baseline.event_cursor)
        .expect("resumed capsule");
    assert!(resumed.window_matched, "{resumed:#?}");
    assert_eq!(resumed.after_cursor, baseline.event_cursor);
    assert_eq!(resumed.current_cursor, live.event_cursor);

    // A cursor without its stream identity refuses to resume: the capsule
    // reconciles to current state and says so.
    let refused = service
        .semantic_capsule(None, live.event_cursor)
        .expect("refused capsule");
    assert!(!refused.window_matched, "{refused:#?}");
    assert!(
        refused
            .limitations
            .iter()
            .any(|limitation| limitation.contains("did not resume")),
        "{refused:#?}"
    );

    // A cursor from a foreign stream also refuses.
    let foreign = service
        .semantic_capsule(Some("mnls-stream-foreign-0"), live.event_cursor)
        .expect("foreign capsule");
    assert!(!foreign.window_matched, "{foreign:#?}");
}

#[test]
fn restart_restores_stream_but_toolchain_change_forces_a_new_epoch() {
    let root = temp_workspace("restart");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let first = LanguageService::new(None);
    first
        .configure_root(Some(root.clone()))
        .expect("first root");
    let uri = workspace_uri(&root, "valid-contracts.mncs");
    first.did_save(&uri, None).expect("first edit");
    let before = first.service_status().expect("before status");

    // Same toolchain restart: stream continuity is restored from the
    // durable checkpoint.
    let second = LanguageService::new(None);
    second
        .configure_root(Some(root.clone()))
        .expect("second root");
    let after = second.service_status().expect("after status");
    assert_eq!(after.stream_identity, before.stream_identity);
    assert!(after.generation >= before.generation);
    assert_ne!(after.service.instance_id, before.service.instance_id);
    assert!(
        after
            .checkpoint
            .as_ref()
            .expect("checkpoint observation")
            .toolchain_matches_current,
        "unchanged toolchain keeps its binding"
    );

    // Toolchain change restart: the cursor must not resume silently.
    std::env::set_var("MNLS_TOOLCHAIN_IDENTITY", "ambient-test-toolchain-rev-2");
    let third = LanguageService::new(None);
    third
        .configure_root(Some(root.clone()))
        .expect("third root");
    let changed = third.service_status().expect("changed status");
    std::env::remove_var("MNLS_TOOLCHAIN_IDENTITY");
    assert_ne!(
        changed.stream_identity, before.stream_identity,
        "toolchain change forces a fresh stream epoch"
    );
    assert!(changed.generation >= before.generation);
    let stale_poll = third.poll_events_for(Some(&before.stream_identity), before.event_cursor, 8);
    assert!(
        stale_poll.reset_required,
        "old stream cursors refuse against the new epoch"
    );
}

#[test]
fn ambient_queries_serve_over_the_resident_socket() {
    let root = temp_workspace("socket");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = Arc::new(LanguageService::new(None));
    service.configure_root(Some(root)).expect("socket root");
    let socket = std::env::temp_dir().join(format!(
        "mncs-language-service-ambient-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
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
    let local = service.service_status().expect("local status");
    let remote_status = remote.service_status().expect("remote status");
    assert_eq!(remote_status.stream_identity, local.stream_identity);
    assert_eq!(remote_status.generation, local.generation);
    assert_eq!(
        remote_status.workspace_root, local.workspace_root,
        "socket clients observe the bound workspace, never a foreign one"
    );
    let capsule = remote
        .semantic_capsule(Some(&local.stream_identity), local.event_cursor)
        .expect("remote capsule");
    assert_eq!(capsule.status, ResponseStatus::Answered, "{capsule:#?}");
    assert!(capsule.window_matched);
}
