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

fn analyze_seeded(service: &LanguageService, root: &std::path::Path, names: &[&str]) {
    for name in names {
        service
            .snapshot(&workspace_uri(root, name))
            .expect("analyze fixture");
    }
}

#[test]
fn service_status_binds_service_workspace_and_toolchain_identity() {
    let root = temp_workspace("status");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = LanguageService::new(None);
    service
        .configure_root(Some(root.clone()))
        .expect("configure root");

    // Status summarizes resident analysis only. Explicitly request the source
    // whose diagnostic evidence this contract test is checking.
    service
        .snapshot(&workspace_uri(&root, "syntax-error.mncs"))
        .expect("analyze syntax-error fixture");

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

    first
        .snapshot(&workspace_uri(&first_root, "valid-contracts.mncs"))
        .expect("analyze first workspace");
    second
        .snapshot(&workspace_uri(&second_root, "syntax-error.mncs"))
        .expect("analyze second workspace");

    let first_status = first.service_status().expect("first status");
    let second_status = second.service_status().expect("second status");
    assert_ne!(first_status.workspace_root, second_status.workspace_root);
    assert_ne!(first_status.stream_identity, second_status.stream_identity);
    assert_eq!(first_status.diagnostics.error, 0);
    assert!(second_status.diagnostics.error > 0);

    // An edit in one workspace advances only its own generation.
    let uri = workspace_uri(&first_root, "valid-contracts.mncs");
    let before = first_status.generation;
    let original = fs::read_to_string(first_root.join("valid-contracts.mncs"))
        .expect("read first workspace source");
    first
        .did_change(
            &uri,
            1,
            original.replace("return next;", "return next + 1;"),
        )
        .expect("change first workspace source");
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
    analyze_seeded(
        &service,
        &root,
        &["valid-contracts.mncs", "syntax-error.mncs"],
    );

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
    analyze_seeded(&service, &root, &["records.mncs"]);

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
fn incomplete_capsule_is_explicit_and_does_not_force_analysis() {
    let root = temp_workspace("capsule-lazy");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = LanguageService::new(None);
    service.configure_root(Some(root)).expect("root");

    let before = service.service_status().expect("status before capsule");
    assert_eq!(before.analysis_pending, 2);
    let capsule = service.semantic_capsule(None, 0).expect("partial capsule");
    assert!(matches!(capsule.status, ResponseStatus::Unsupported { .. }));
    assert_eq!(capsule.measured.analysis_pending_documents, 2);
    let after = service.service_status().expect("status after capsule");
    assert_eq!(after.analysis_pending, before.analysis_pending);
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
fn disk_edit_then_removal_emits_complete_superseding_semantic_state() {
    let root = temp_workspace("disk-removal");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let service = LanguageService::new(None);
    service
        .configure_root(Some(root.clone()))
        .expect("configure root");
    let uri = workspace_uri(&root, "valid-contracts.mncs");
    service.snapshot(&uri).expect("baseline analysis");
    let baseline = service.service_status().expect("baseline status");

    let path = root.join("valid-contracts.mncs");
    let original = fs::read_to_string(&path).expect("read source");
    fs::write(&path, original.replace("return next;", "return next + 1;"))
        .expect("write source edit");
    assert_eq!(
        service
            .refresh_workspace()
            .expect("publish disk edit")
            .len(),
        1
    );

    fs::remove_file(&path).expect("remove source");
    assert_eq!(
        service
            .refresh_workspace()
            .expect("publish disk removal")
            .len(),
        1
    );
    let status = service.service_status().expect("post-removal status");
    assert_eq!(status.documents, 0);
    let window = service.poll_events_for(Some(&baseline.stream_identity), 0, 8);
    assert!(!window.reset_required, "both changes remain replayable");
    assert_eq!(window.events.len(), 2);
    assert!(!window.events[0].removed);
    let removal = &window.events[1];
    assert!(removal.removed);
    assert!(removal.reconciled);
    assert_eq!(removal.supersedes_through_cursor, window.events[0].cursor);
    assert!(removal.impact_complete, "removal covers its resident graph");
    assert!(removal.obligations.complete);
    assert!(removal
        .semantic_subjects
        .iter()
        .all(|subject| subject.change == "removed"));
}

#[test]
fn first_editor_change_analyzes_the_existing_disk_side_before_mutation() {
    let root = temp_workspace("first-editor-change");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let service = LanguageService::new(None);
    service
        .configure_root(Some(root.clone()))
        .expect("configure root");
    let uri = workspace_uri(&root, "valid-contracts.mncs");
    let original = fs::read_to_string(root.join("valid-contracts.mncs")).expect("read source");
    let before = service.service_status().expect("baseline status");

    service
        .did_change(
            &uri,
            1,
            original.replace("return next;", "return next + 1;"),
        )
        .expect("first editor change");
    let window = service.poll_events_for(Some(&before.stream_identity), before.event_cursor, 8);
    assert!(!window.reset_required);
    assert_eq!(window.events.len(), 1);
    assert!(
        window.events[0].impact_complete,
        "both semantic sides are known"
    );
    assert!(window.events[0].obligations.complete);
}

#[test]
fn startup_reconciles_multiple_changed_documents_into_one_complete_checkpoint() {
    let root = temp_workspace("startup-batched-reconcile");
    seed_workspace(&root, &["valid-contracts.mncs", "finite-match.mncs"]);
    let initial = LanguageService::new(None);
    initial
        .configure_root(Some(root.clone()))
        .expect("initial root");
    let initial_status = initial.service_status().expect("initial status");
    assert_eq!(initial_status.event_cursor, 0);

    let contracts_path = root.join("valid-contracts.mncs");
    let contracts = fs::read_to_string(&contracts_path).expect("read contracts");
    fs::write(
        &contracts_path,
        contracts.replace("return next;", "return next + 1;"),
    )
    .expect("edit contracts on disk");
    let finite_path = root.join("finite-match.mncs");
    let finite = fs::read_to_string(&finite_path).expect("read finite match");
    fs::write(&finite_path, finite.replace("score >= 50", "score >= 51"))
        .expect("edit finite match on disk");

    let resumed = LanguageService::new(None);
    resumed
        .configure_root(Some(root.clone()))
        .expect("reconcile changed documents");
    let status = resumed.service_status().expect("resumed status");
    assert_eq!(status.event_cursor, 2);
    let replay = resumed.poll_events_for(Some(&status.stream_identity), 0, 8);
    assert!(!replay.reset_required);
    assert_eq!(replay.events.len(), 2);
    assert!(replay.events.iter().all(|event| event.reconciled));
    let event_uris: std::collections::BTreeSet<_> = replay
        .events
        .iter()
        .map(|event| event.current.uri.as_str())
        .collect();
    assert_eq!(
        event_uris,
        [
            workspace_uri(&root, "finite-match.mncs"),
            workspace_uri(&root, "valid-contracts.mncs"),
        ]
        .iter()
        .map(String::as_str)
        .collect()
    );

    let checkpoint_path = root
        .join(".mncs")
        .join("mnls-language-service.checkpoint.json");
    let checkpoint: serde_json::Value =
        serde_json::from_slice(&fs::read(checkpoint_path).expect("read persisted checkpoint"))
            .expect("decode persisted checkpoint");
    assert_eq!(checkpoint["last_cursor"], 2);
    assert_eq!(checkpoint["documents"].as_object().unwrap().len(), 2);
    for name in ["finite-match.mncs", "valid-contracts.mncs"] {
        let uri = workspace_uri(&root, name);
        assert_eq!(
            checkpoint["documents"][&uri],
            resumed
                .content_fingerprint(&uri)
                .expect("current content identity")
        );
    }
}

#[test]
fn restart_restores_stream_but_toolchain_change_forces_a_new_epoch() {
    let root = temp_workspace("restart");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let first = LanguageService::new(None);
    first
        .configure_root(Some(root.clone()))
        .expect("first root");
    let path = root.join("valid-contracts.mncs");
    let original = fs::read_to_string(&path).expect("read source");
    fs::write(&path, original.replace("return next;", "return next + 1;"))
        .expect("write source edit");
    assert_eq!(first.refresh_workspace().expect("publish edit").len(), 1);
    let before = first.service_status().expect("before status");
    assert_eq!(before.event_cursor, 1);

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
    let resumed = second.poll_events_for(Some(&before.stream_identity), before.event_cursor, 8);
    assert!(
        !resumed.reset_required,
        "acknowledged cursor resumes in the same stream"
    );
    assert!(
        resumed.events.is_empty(),
        "acknowledged work is not replayed as new"
    );
    assert_eq!(resumed.current_cursor, before.event_cursor);
    if before.event_cursor > 0 {
        let behind =
            second.poll_events_for(Some(&before.stream_identity), before.event_cursor - 1, 8);
        assert!(
            !behind.reset_required,
            "the durable event window resumes exactly across a service restart"
        );
        assert_eq!(behind.events.len(), 1);
        assert_eq!(behind.events[0].cursor, before.event_cursor);
    }

    // A different Language Service executable starts a new semantic epoch;
    // stale event cursors are never rebound to changed provider semantics.
    let checkpoint_path = root
        .join(".mncs")
        .join("mnls-language-service.checkpoint.json");
    let mut checkpoint: serde_json::Value =
        serde_json::from_slice(&fs::read(&checkpoint_path).expect("read checkpoint"))
            .expect("decode checkpoint");
    checkpoint["service_build_fingerprint"] =
        serde_json::Value::String("sha256:stale-service-build".to_owned());
    fs::write(
        &checkpoint_path,
        serde_json::to_vec_pretty(&checkpoint).expect("encode checkpoint"),
    )
    .expect("write stale build identity");
    let rebuilt = LanguageService::new(None);
    rebuilt
        .configure_root(Some(root.clone()))
        .expect("new service build root");
    let rebuilt_status = rebuilt.service_status().expect("new service status");
    assert_ne!(rebuilt_status.stream_identity, before.stream_identity);
    assert_eq!(rebuilt_status.event_cursor, 0);

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
fn semantic_impact_combines_edges_neighborhood_and_obligations() {
    let root = temp_workspace("impact");
    seed_workspace(&root, &["valid-contracts.mncs"]);
    let service = LanguageService::new(None);
    service
        .configure_root(Some(root.clone()))
        .expect("impact root");
    let uri = workspace_uri(&root, "valid-contracts.mncs");
    let text = fs::read_to_string(root.join("valid-contracts.mncs")).expect("read");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn caller").expect("needle") + "fn ".len();
    let position = map.position_of(&text, offset);
    let described = service
        .describe_position(&uri, position.line, position.character)
        .expect("describe");
    let identity = described
        .subject
        .expect("subject")
        .summary
        .identity
        .expect("identity");

    let impact = service
        .semantic_impact(&uri, &identity)
        .expect("semantic impact");
    assert_eq!(
        impact.schema_version,
        mncs_service_core::SEMANTIC_IMPACT_SCHEMA_VERSION
    );
    assert_eq!(impact.status, ResponseStatus::Answered, "{impact:#?}");
    assert_eq!(impact.subject_identity, identity);
    assert!(impact.snapshot.expect("snapshot").current);
    let neighborhood = impact.impact.expect("neighborhood");
    assert!(
        neighborhood
            .nodes
            .iter()
            .any(|node| node.identity.0 == identity),
        "neighborhood is rooted at the subject"
    );
    assert!(
        !impact.dependencies.outgoing.is_empty() || !impact.dependents.incoming.is_empty(),
        "caller participates in call edges"
    );
    for obligation in &impact.affected_obligations {
        assert!(
            obligation.subject == identity
                || neighborhood
                    .nodes
                    .iter()
                    .any(|node| node.identity.0 == obligation.subject
                        || node.identity.0 == obligation.identity),
            "affected obligations fall inside the neighborhood"
        );
    }
}

#[test]
fn ambient_queries_serve_over_the_resident_socket() {
    let root = temp_workspace("socket");
    seed_workspace(&root, &["valid-contracts.mncs", "syntax-error.mncs"]);
    let service = Arc::new(LanguageService::new(None));
    service
        .configure_root(Some(root.clone()))
        .expect("socket root");
    analyze_seeded(
        &service,
        &root,
        &["valid-contracts.mncs", "syntax-error.mncs"],
    );
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
