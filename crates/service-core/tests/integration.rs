//! Integration tests exercising the resident core as a language service:
//! snapshots, invalidation, navigation, diagnostics, obligations, tokens,
//! completion, and failure behavior against representative MNCS fixtures.

use mncs_service_core::{
    serve_unix, DebugCapabilityStatus, LanguageService, LanguageServiceClient,
    RemoteLanguageService, ResponseStatus, SymbolKind,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn language_library() -> PathBuf {
    let root = std::env::var_os("MNCS_LANGUAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-language")
        });
    root.join("library")
}

fn fixture_uri(name: &str) -> String {
    format!("file://{}", fixtures_dir().join(name).display())
}

fn service() -> LanguageService {
    LanguageService::new(Some(fixtures_dir()))
}

const CONTRACTS: &str = "valid-contracts.mncs";

#[test]
fn workspace_status_reports_documents_and_readiness() {
    let svc = service();
    svc.discover_workspace().expect("discovery");
    let status = svc.workspace_status().expect("workspace status");
    assert!(status
        .documents
        .iter()
        .any(|document| document.uri.ends_with(CONTRACTS)));
    let entry = status
        .documents
        .iter()
        .find(|document| document.uri.ends_with(CONTRACTS))
        .expect("fixture present");
    assert!(!entry.open);
}

#[test]
fn filesystem_refresh_projects_complete_impact_from_resident_before_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "mncs-language-service-refresh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary workspace");
    let path = root.join("valid-contracts.mncs");
    fs::copy(fixtures_dir().join(CONTRACTS), &path).expect("fixture copy");
    let uri = format!("file://{}", path.display());
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("discovery");
    service
        .document_diagnostics(&uri)
        .expect("baseline analysis");

    let original = fs::read_to_string(&path).expect("read fixture copy");
    fs::write(&path, original.replace("return next;", "return next + 1;"))
        .expect("edit fixture copy");
    service.refresh_workspace().expect("filesystem refresh");

    let event = service
        .poll_events(0, 8)
        .events
        .into_iter()
        .last()
        .expect("filesystem change event");
    assert!(
        event.impact_complete,
        "resident before snapshot must enable impact"
    );
    assert!(event.impact.is_some());
    assert!(!event.semantic_subjects.is_empty());
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn restart_reconciles_offline_edit_without_reusing_cursor_alone() {
    let root = std::env::temp_dir().join(format!(
        "mncs-language-service-checkpoint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary workspace");
    let path = root.join(CONTRACTS);
    fs::copy(fixtures_dir().join(CONTRACTS), &path).expect("fixture copy");

    let first = LanguageService::new(None);
    first
        .configure_root(Some(root.clone()))
        .expect("initial checkpoint baseline");
    let first_status = first.workspace_status().expect("initial status");
    assert!(!first_status.stream_identity.is_empty());
    assert_eq!(first_status.event_cursor, 0);
    assert!(root
        .join(".mncs/mnls-language-service.checkpoint.json")
        .is_file());

    let original = fs::read_to_string(&path).expect("read fixture copy");
    fs::write(&path, original.replace("return next;", "return next + 1;")).expect("offline edit");

    let second = LanguageService::new(None);
    second
        .configure_root(Some(root.clone()))
        .expect("reconcile offline edit");
    let status = second.workspace_status().expect("reconciled status");
    assert_eq!(status.stream_identity, first_status.stream_identity);
    assert!(status.event_cursor > first_status.event_cursor);
    let cursor = second.poll_events_for(
        Some(&first_status.stream_identity),
        first_status.event_cursor,
        8,
    );
    assert!(!cursor.reset_required);
    let event = cursor.events.last().expect("offline reconciliation event");
    assert!(event.reconciled);
    assert!(!event.impact_complete);
    assert_eq!(event.stream_identity, first_status.stream_identity);
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn resident_socket_clients_share_generations_and_event_cursor() {
    let service = Arc::new(LanguageService::default());
    let socket = std::env::temp_dir().join(format!(
        "mncs-language-service-test-{}-{}.sock",
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
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(socket.exists(), "resident socket did not start");

    let remote = RemoteLanguageService::connect_path(&socket);
    let uri = "untitled:shared-resident-state";
    let first_generation = remote
        .did_open(
            uri,
            1,
            "mncs 0.17;\nmodule examples.shared;\ntest sample() -> (result: i64) { return 1; }\n"
                .to_owned(),
        )
        .expect("remote open");
    let second_generation = remote
        .did_change_incremental(
            uri,
            2,
            vec![mncs_service_core::TextChange {
                range: None,
                text: "mncs 0.17;\nmodule examples.shared;\ntest sample() -> (result: i64) { return 2; }\n"
                    .to_owned(),
            }],
        )
        .expect("remote change");
    assert!(second_generation > first_generation);

    let local_status = service.workspace_status().expect("local status");
    let remote_status = remote.workspace_status().expect("remote status");
    assert_eq!(local_status.generation, remote_status.generation);
    assert_eq!(local_status.event_cursor, remote_status.event_cursor);
    assert_eq!(remote_status.generation, second_generation);

    let remote_events = remote.poll_events(0, 16);
    let local_events = service.poll_events(0, 16);
    assert_eq!(remote_events.current_cursor, local_events.current_cursor);
    assert_eq!(remote_events.events.len(), 2);
    assert_eq!(
        remote_events
            .events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>(),
        local_events
            .events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        remote_events.events[1].replaced_generation,
        first_generation
    );
    assert_eq!(
        remote_events.events[1].current_generation,
        second_generation
    );
    assert_eq!(
        remote.content(uri).expect("remote content"),
        service.content(uri).expect("local content")
    );
    assert_eq!(
        remote_events.events[1].current.identity,
        service
            .snapshot(uri)
            .expect("shared snapshot")
            .source_identity
    );
}

#[test]
fn snapshot_identity_binds_to_exact_content_and_is_reused() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let first = svc.snapshot(&uri).expect("snapshot");
    assert!(first.valid());
    assert!(first.source_identity.starts_with("mncs:source:artifact:"));

    let second = svc.snapshot(&uri).expect("snapshot");
    assert!(
        Arc::ptr_eq(&first, &second),
        "unchanged documents must reuse the resident snapshot"
    );

    // An unsaved buffer change produces a new identity-bound snapshot.
    svc.did_open(
        &uri,
        2,
        first.text().replace(
            "return bounded_step(value, value);",
            "return missing_fn(value, value);",
        ),
    )
    .expect("open with buffer");
    let third = svc.snapshot(&uri).expect("snapshot");
    assert_ne!(first.source_identity, third.source_identity);
    assert!(
        !third.valid(),
        "edited-away binding must surface as invalid"
    );
}

#[test]
fn debug_source_binding_reuses_compiler_test_identity_and_stable_span() {
    let svc = LanguageService::default();
    let uri = "untitled:debug-source-binding";
    svc.did_open(
        uri,
        1,
        "mncs 0.17;\nmodule examples.debug;\ntest sample() -> (result: i64) { return 42; }\n"
            .to_owned(),
    )
    .expect("open source");

    let response = svc
        .debug_source_binding(uri, None, Some(2), Some(6))
        .expect("debug binding");
    assert_eq!(response.status, ResponseStatus::Answered);
    let binding = response.binding.expect("binding");
    assert_eq!(binding.schema_version, "mncs.debug-source-binding/1");
    assert!(binding.source_identity.starts_with("mncs:source:artifact:"));
    assert_eq!(binding.module_name, "examples.debug");
    assert!(binding.module_identity.starts_with("mncs:"));
    assert_eq!(binding.function_name.as_deref(), Some("sample"));
    assert!(binding.function_identity.is_some());
    assert!(binding.test_declaration_identity.is_some());
    assert!(binding.test_case_identity.is_some());
    assert_eq!(binding.source_span.start_line, 2);
    assert_eq!(binding.failure_location, None);
    assert_eq!(
        binding.runtime_operation_resolution.status,
        DebugCapabilityStatus::Unsupported
    );
    assert_eq!(
        binding.breakpoint_resolution.status,
        DebugCapabilityStatus::Unsupported
    );

    let by_test_case = svc
        .debug_source_binding(uri, binding.test_case_identity.as_deref(), None, None)
        .expect("test case binding");
    assert_eq!(
        by_test_case.binding.expect("binding").source_identity,
        binding.source_identity
    );
}

#[test]
fn debug_source_binding_resolves_compiler_operation_span() {
    let svc = LanguageService::default();
    let uri = "untitled:debug-operation-binding";
    svc.did_open(
        uri,
        1,
        "mncs 0.17;\nmodule examples.debug;\n\nfn increment(value: i64) -> (result: i64) {\n    return value + 1;\n}\n\nfn wrapper(value: i64) -> (result: i64) {\n    return increment(value);\n}\n"
            .to_owned(),
    )
    .expect("open source");

    let snapshot = svc.snapshot(uri).expect("snapshot");
    let source_map = snapshot
        .front_end
        .execution_source_map
        .as_ref()
        .expect("execution source map");
    let operation = source_map
        .operations
        .iter()
        .find(|operation| operation.source_span.is_some())
        .expect("source-backed operation");
    let operation_identity = operation.identity.0.clone();
    let operation_span = operation.source_span.expect("operation span");

    let response = svc
        .debug_source_binding(uri, Some(&operation_identity), None, None)
        .expect("operation binding");
    let binding = response.binding.expect("binding");
    assert_eq!(
        binding.runtime_operation_identity.as_deref(),
        Some(operation_identity.as_str())
    );
    let runtime_span = binding
        .runtime_operation_source_span
        .expect("runtime operation source span");
    assert_eq!(runtime_span.start_byte, operation_span.start);
    assert_eq!(runtime_span.end_byte, operation_span.end);
    assert_eq!(
        binding.runtime_operation_resolution.status,
        DebugCapabilityStatus::Supported
    );
    assert_eq!(
        binding.breakpoint_resolution.status,
        DebugCapabilityStatus::PartiallySupported
    );
}

#[test]
fn diagnostics_preserve_authoritative_codes_stages_and_spans() {
    let svc = service();
    let response = svc
        .document_diagnostics(&fixture_uri("syntax-error.mncs"))
        .expect("diagnostics");
    assert_eq!(response.items.len(), 1, "{:#?}", response.items);
    let diagnostic = &response.items[0];
    assert_eq!(diagnostic.code, "MNP016");
    assert_eq!(diagnostic.stage, "parsing");
    assert_eq!(diagnostic.severity, "error");
    assert!(diagnostic.range.end_byte > diagnostic.range.start_byte);

    // Semantic (elaboration) errors keep their codes too.
    let semantic = svc
        .document_diagnostics(&fixture_uri("semantic-error.mncs"))
        .expect("diagnostics");
    assert!(
        semantic
            .items
            .iter()
            .any(|item| item.code == "MNCS010" || item.code == "MNE111"),
        "expected effect/capability diagnostic: {:#?}",
        semantic.items
    );
}

#[test]
fn subjects_at_position_resolves_declaration_with_identity() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let needle = text.find("bounded_step").expect("needle");
    let map = mncs_service_core::PositionMap::new(&text);
    let position = map.position_of(&text, needle);

    let response = svc
        .subjects_at(&uri, position.line, position.character)
        .expect("subjects");
    assert_eq!(response.status, ResponseStatus::Answered);
    assert_eq!(response.occurrences.len(), 1);
    let occurrence = &response.occurrences[0];
    assert_eq!(
        occurrence.role,
        mncs_service_core::OccurrenceRole::Declaration
    );
    assert_eq!(occurrence.symbol.kind, SymbolKind::Function);
    let identity = occurrence.symbol.identity.as_deref().expect("identity");
    assert_eq!(
        identity,
        "mncs:0.2:function:examples.contracts::bounded_step"
    );
}

#[test]
fn definition_and_references_navigate_via_resolution_not_grep() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);

    // The call site inside `caller`.
    let call_offset = text.rfind("bounded_step").expect("call site");
    let call_position = map.position_of(&text, call_offset);

    let definition = svc
        .definition(&uri, call_position.line, call_position.character)
        .expect("definition");
    assert_eq!(definition.status, ResponseStatus::Answered);
    assert_eq!(definition.definitions.len(), 1);
    assert_eq!(definition.definitions[0].name, "bounded_step");
    assert_eq!(
        definition.definitions[0].range.start_line,
        map.position_of(&text, text.find("fn bounded_step").expect("decl"))
            .line
    );

    let references = svc
        .references(&uri, call_position.line, call_position.character, true)
        .expect("references");
    assert_eq!(references.status, ResponseStatus::Answered);
    assert_eq!(references.hits.len(), 2, "declaration + single call site");
    assert!(references.hits.iter().any(|hit| hit.is_declaration));

    // Positions without resolvable subjects must be explicit, not empty success.
    let none = svc.subjects_at(&uri, 0, 0).expect("subjects");
    assert!(matches!(none.status, ResponseStatus::Unresolved { .. }));
}

#[test]
fn hover_renders_signature_contracts_and_identity() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn bounded_step").expect("needle") + "fn ".len();

    let position = map.position_of(&text, offset);
    let hover = svc
        .hover(&uri, position.line, position.character)
        .expect("hover");
    assert_eq!(hover.status, ResponseStatus::Answered);
    let markdown = hover.markdown.expect("markdown");
    assert!(
        markdown.contains("fn bounded_step(n: i64, limit: i64) -> (result: i64)"),
        "{markdown}"
    );
    assert!(markdown.contains("requires"), "{markdown}");
    assert!(markdown.contains("checked_integer"), "{markdown}");
    assert!(markdown.contains("mncs:0.2:function:"), "{markdown}");

    let subject = hover.subject.expect("subject");
    assert_eq!(subject.kind, SymbolKind::Function);
}

#[test]
fn describe_returns_structured_semantics_for_machines() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn caller").expect("needle") + "fn ".len();
    let position = map.position_of(&text, offset);

    let described = svc
        .describe_position(&uri, position.line, position.character)
        .expect("describe");
    let subject = described.subject.as_ref().expect("subject");
    assert_eq!(subject.summary.name, "caller");
    assert!(subject.capabilities.contains(&"checked_integer".to_owned()));
    assert!(subject.calls_outgoing >= 1, "caller calls bounded_step");

    // Same subject through its identity yields the same description.
    let identity = subject.summary.identity.clone().expect("identity");
    let by_identity = svc.describe_identity(&uri, &identity).expect("by identity");
    assert_eq!(
        by_identity.subject.as_ref().expect("subject").summary.range,
        subject.summary.range,
        "LSP and MCP paths resolve the same semantic state"
    );
}

#[test]
fn dependencies_and_dependents_use_the_semantic_graph() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);

    let callee_offset = text.find("fn bounded_step").expect("callee") + "fn ".len();
    let callee_position = map.position_of(&text, callee_offset);
    let described = svc
        .describe_position(&uri, callee_position.line, callee_position.character)
        .expect("describe");
    let identity = described
        .subject
        .unwrap()
        .summary
        .identity
        .expect("identity");

    let dependents = svc.dependents(&uri, &identity).expect("dependents");
    assert_eq!(dependents.status, ResponseStatus::Answered);
    assert!(dependents
        .incoming
        .iter()
        .any(|edge| edge.name.as_deref() == Some("caller")));

    let dependencies = svc.dependencies(&uri, &identity).expect("dependencies");
    assert!(dependencies.outgoing.is_empty());

    // Unknown identities fail closed.
    let missing = svc
        .dependencies(&uri, "mncs:0.2:function::nowhere::nope")
        .expect("query ran");
    assert!(matches!(missing.status, ResponseStatus::Unresolved { .. }));
}

#[test]
fn obligations_preserve_pass_fail_unknown() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let response = svc.obligations(&uri, None).expect("obligations");
    assert_eq!(response.status, ResponseStatus::Answered);
    assert!(
        response.counts.unknown > 0,
        "contract without evidence stays UNKNOWN"
    );
    assert!(
        response.counts.pass > 0,
        "authority closure is PASS where the language proves it"
    );
    assert_eq!(response.counts.fail, 0);
    for obligation in &response.obligations {
        assert!(matches!(
            obligation.status.as_str(),
            "pass" | "fail" | "unknown"
        ));
        assert!(obligation.identity.starts_with("mncs:"));
    }

    // Filtering by subject keeps only related obligations.
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn caller").expect("needle") + "fn ".len();
    let position = map.position_of(&text, offset);
    let described = svc
        .describe_position(&uri, position.line, position.character)
        .expect("describe");
    let identity = described.subject.unwrap().summary.identity.unwrap();
    let filtered = svc.obligations(&uri, Some(&identity)).expect("filtered");
    assert!(!filtered.obligations.is_empty());
    assert!(filtered.obligations.len() < response.obligations.len());
}

#[test]
fn native_obligations_differentially_agree_with_rust_control() {
    let library = language_library();
    std::env::set_var("MNCS_LIBRARY_PATH", library);

    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let response = svc
        .native_obligations(&uri, None)
        .expect("native obligations");
    assert_eq!(response.status, ResponseStatus::Answered, "{response:#?}");
    assert_eq!(response.counts, response.reference_counts);
    let native = response.native.expect("native execution evidence");
    assert_eq!(native.backend, "mncs-research-bytecode");
    assert_eq!(native.input_count, response.obligations.len());
    assert_eq!(native.observed_count, response.obligations.len());
    assert!(native.valid);
    assert!(native.pass_count > 0);
    assert!(native.unknown_count > 0);
    assert_eq!(native.dominant_status, "unknown");

    // The frozen artifact is reused for an unchanged dependency/source pair.
    let again = svc
        .native_obligations(&uri, None)
        .expect("native obligations reuse");
    assert_eq!(
        again
            .native
            .expect("native execution evidence")
            .kernel_artifact_identity,
        native.kernel_artifact_identity
    );
}

#[test]
fn native_obligations_preserves_empty_unknown_summary() {
    let library = language_library();
    std::env::set_var("MNCS_LIBRARY_PATH", library);

    let svc = service();
    let response = svc
        .native_obligations(&fixture_uri("records.mncs"), None)
        .expect("native empty obligations");
    assert_eq!(response.status, ResponseStatus::Answered, "{response:#?}");
    assert_eq!(response.reference_counts, Default::default());
    let native = response.native.expect("native execution evidence");
    assert_eq!(native.input_count, 0);
    assert_eq!(native.observed_count, 0);
    assert_eq!(native.unknown_count, 0);
    assert_eq!(native.dominant_status, "unknown");
    assert!(native.valid);
}

#[test]
fn document_symbols_include_profile_05_records_and_fields() {
    let svc = service();
    let uri = fixture_uri("records.mncs");
    let response = svc.document_symbols(&uri).expect("symbols");
    assert_eq!(response.status, ResponseStatus::Answered);

    fn collect(node: &mncs_service_core::DocumentSymbolNode, out: &mut Vec<(String, SymbolKind)>) {
        out.push((node.summary.name.clone(), node.summary.kind));
        node.children.iter().for_each(|child| collect(child, out));
    }
    let mut flat = Vec::new();
    for root in &response.symbols {
        collect(root, &mut flat);
    }
    assert!(flat
        .iter()
        .any(|(name, kind)| name == "Reading" && *kind == SymbolKind::RecordType));
    assert!(flat
        .iter()
        .any(|(name, kind)| name == "celsius" && *kind == SymbolKind::RecordField));
    assert!(flat
        .iter()
        .any(|(name, kind)| name == "adjust" && *kind == SymbolKind::Function));
    assert!(flat
        .iter()
        .any(|(name, kind)| name == "base" && *kind == SymbolKind::Binding));

    // Workspace symbols see the same inventory.
    let workspace = svc.workspace_symbols("Reading");
    assert!(workspace
        .symbols
        .iter()
        .any(|hit| hit.summary.name == "Reading"));
}

#[test]
fn document_symbols_are_unsupported_without_an_ast() {
    let svc = service();
    let uri = fixture_uri("syntax-error.mncs");
    let response = svc.document_symbols(&uri).expect("symbols query runs");
    assert!(matches!(
        response.status,
        ResponseStatus::Unsupported { .. }
    ));
}

#[test]
fn finite_types_expose_variants_through_describe() {
    let svc = service();
    let uri = fixture_uri("finite-match.mncs");
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("enum Verdict").expect("needle") + "enum ".len();
    let position = map.position_of(&text, offset);

    let described = svc
        .describe_position(&uri, position.line, position.character)
        .expect("describe");
    let subject = described.subject.expect("subject");
    assert_eq!(subject.summary.kind, SymbolKind::FiniteType);
    let variant_names: Vec<_> = subject
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    assert_eq!(variant_names, vec!["Pass", "Fail", "Skip"]);
    assert!(subject
        .members
        .iter()
        .all(|member| member.identity.is_some()));
}

#[test]
fn semantic_tokens_classify_only_what_is_authoritative() {
    let svc = service();
    let uri = fixture_uri("unicode.mncs");
    let response = svc.semantic_tokens(&uri).expect("tokens");
    let classes: Vec<_> = response.tokens.iter().map(|token| token.class).collect();

    use mncs_service_core::TokenClass::*;
    assert!(classes.contains(&Keyword));
    assert!(
        classes.contains(&Module),
        "module name after `module` is classified"
    );
    assert!(
        classes.contains(&Function),
        "resolved function names are classified"
    );
    assert!(classes.contains(&Parameter));

    // Unicode identifiers must classify without breaking coordinates.
    let function_token = response
        .tokens
        .iter()
        .find(|token| token.class == Function)
        .expect("function token");
    assert!(function_token.length_utf16 > 0);
}

#[test]
fn unicode_positions_round_trip_through_utf16() {
    let svc = service();
    let uri = fixture_uri("unicode.mncs");
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);

    // Cursor after the multibyte identifier `évaluate`.
    let offset = text.find("évaluate").expect("needle") + 'é'.len_utf8();
    let position = map.position_of(&text, offset);
    // 'é' contributes exactly one UTF-16 unit.
    let plain = "fn ".len() as u32;
    assert_eq!(position.character, plain + 1);

    let hover = svc
        .hover(&uri, position.line, position.character)
        .expect("hover");
    assert_eq!(hover.status, ResponseStatus::Answered);
    assert_eq!(hover.subject.expect("subject").name, "évaluate");
}

#[test]
fn completion_is_conservative_but_useful() {
    let svc = service();
    let uri = fixture_uri("finite-match.mncs");
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);

    // Prefix completion for a keyword.
    let prefix_offset = text.find("return match").expect("needle") + 2;
    let position = map.position_of(&text, prefix_offset);
    let items = svc
        .completion(&uri, position.line, position.character + 1)
        .expect("completion");
    assert!(
        items
            .items
            .iter()
            .any(|candidate| candidate.label == "return"),
        "keyword completed from prefix: {:#?}",
        items.items
    );

    // Member completion on a nominal type constructor namespace.
    let variant_offset = text.find("Verdict.Pass").expect("constructor");
    let dot_column = text[variant_offset..].find('.').expect("dot") + variant_offset;
    let dot_position = map.position_of(&text, dot_column);
    let members = svc
        .completion(&uri, dot_position.line, dot_position.character + 1)
        .expect("members");
    assert!(
        members
            .items
            .iter()
            .any(|candidate| candidate.label == "Pass"),
        "variants offered after Type.: {:#?}",
        members.items
    );
}

#[test]
fn folding_ranges_cover_functions_and_blocks() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let response = svc.folding_ranges(&uri).expect("folding");
    assert!(response.ranges.len() >= 2);
}

#[test]
fn highlights_cover_declaration_and_references() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("let next").expect("binding") + "let ".len();
    let position = map.position_of(&text, offset);
    let highlights = svc
        .highlights(&uri, position.line, position.character)
        .expect("highlights");
    assert!(highlights.ranges.len() >= 2, "declaration plus uses");
}

#[test]
fn unsaved_buffers_drive_analysis_until_close() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);

    // Break the file in an unsaved buffer: diagnostics must reflect it.
    let original = (*svc.store().content(&uri).expect("content")).clone();
    let mut edited = original.clone();
    edited.push_str("\nfn broken(x: i64) -> (result: i64) {\n    return ghost;\n}\n");
    svc.did_open(&uri, 5, edited).expect("open");
    let broken = svc.document_diagnostics(&uri).expect("diagnostics");
    assert!(
        broken.items.iter().any(|item| item.code == "MNE102"),
        "buffer state analyzed: {:#?}",
        broken.items
    );

    // Restore the buffer: clean again.
    svc.did_change(&uri, 6, original.clone()).expect("change");
    let fixed = svc.document_diagnostics(&uri).expect("diagnostics");
    assert!(fixed.items.is_empty(), "{:#?}", fixed.items);

    // Close reverts to disk content.
    svc.did_close(&uri).expect("close");
    let disk = svc.document_diagnostics(&uri).expect("diagnostics");
    assert!(disk.items.is_empty());
    assert_eq!(
        (*svc.store().content(&uri).expect("content")).clone(),
        original
    );
}

#[test]
fn repeated_queries_after_changes_do_not_return_stale_results() {
    let svc = service();
    let uri = fixture_uri("bounded-iteration.mncs");
    let before = svc.document_symbols(&uri).expect("symbols");
    assert_eq!(before.status, ResponseStatus::Answered);

    let text = (*svc.store().content(&uri).expect("content")).clone();
    svc.did_change(&uri, 9, text.replace("up_to 4", "up_to 2"))
        .expect("change");

    let after = svc.document_symbols(&uri).expect("symbols after change");
    assert_eq!(after.status, ResponseStatus::Answered);

    // Snapshot info must reflect the new content identity, not the old one.
    let diagnostics = svc.document_diagnostics(&uri).expect("diagnostics");
    let info = diagnostics.snapshot.expect("snapshot info");
    assert_eq!(info.uri, uri);
    let fresh = svc.content_fingerprint(&uri).expect("fingerprint");
    assert_eq!(
        info.source_identity, fresh,
        "responses are computed against current content"
    );
}

#[test]
fn unknown_documents_fail_explicitly() {
    let svc = service();
    let error = svc
        .document_diagnostics("file:///definitely/not/here.mncs")
        .expect_err("must error");
    assert!(matches!(
        error,
        mncs_service_core::ServiceError::DocumentNotFound { .. }
    ));
}

#[test]
fn context_packet_is_bounded_and_honest_about_completeness() {
    let svc = service();
    let uri = fixture_uri(CONTRACTS);
    let text = svc.store().content(&uri).expect("content");
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("fn caller").expect("needle") + "fn ".len();
    let position = map.position_of(&text, offset);
    let described = svc
        .describe_position(&uri, position.line, position.character)
        .expect("describe");
    let identity = described.subject.unwrap().summary.identity.unwrap();

    let packet = svc.context_packet(&uri, &identity, 1).expect("packet");
    assert_eq!(packet.status, ResponseStatus::Answered);
    assert!(!packet.excerpts.is_empty());
    assert!(packet.excerpts.len() <= 2, "budget respected");
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).expect("fixture text")
}

#[test]
fn candidate_analysis_reports_identity_bound_deltas_without_touching_the_baseline() {
    let uri = fixture_uri("records.mncs");
    let baseline_text = read_fixture("records.mncs");
    let service = service();
    // Publish the baseline through the ordinary lifecycle.
    service
        .did_open(&uri, 1, baseline_text.clone())
        .expect("open");

    // Candidate: change a function body so its identity fingerprint changes.
    let candidate_text =
        baseline_text.replace("return updated.celsius;", "return updated.celsius + 1;");
    assert_ne!(candidate_text, baseline_text, "fixture must be changeable");

    let response = service
        .analyze_candidate(&uri, &candidate_text)
        .expect("candidate analysis");
    assert_eq!(response.status, ResponseStatus::Answered);
    assert!(response.changed);
    assert_ne!(
        response.baseline_source_identity,
        response.candidate_source_identity
    );
    let semantic = response.semantic.as_ref().expect("both sides elaborate");
    assert!(
        !semantic.changed.is_empty() || !semantic.added.is_empty(),
        "a body change must touch at least one semantic identity: {semantic:#?}"
    );

    // The workspace baseline is untouched: the resident snapshot still
    // carries the baseline source identity.
    let resident = service.snapshot(&uri).expect("resident snapshot");
    assert_eq!(resident.source_identity, response.baseline_source_identity);
}

#[test]
fn candidate_analysis_refuses_stale_and_identical_candidates_fail_closed() {
    let uri = fixture_uri("records.mncs");
    let baseline_text = read_fixture("records.mncs");
    let service = service();
    service
        .did_open(&uri, 1, baseline_text.clone())
        .expect("open");

    let unchanged = service
        .analyze_candidate(&uri, &baseline_text)
        .expect("identical");
    assert!(!unchanged.changed);
    assert!(unchanged
        .unresolved
        .iter()
        .any(|note| note.contains("identical")));

    // A candidate that does not parse answers with diagnostics only and an
    // explicit unresolved note instead of guessing.
    let broken = format!("{}\nfn broken( {{", baseline_text);
    let response = service
        .analyze_candidate(&uri, &broken)
        .expect("broken candidate");
    assert!(!response.candidate_elaborates);
    assert!(response.semantic.is_none());
    assert!(response
        .unresolved
        .iter()
        .any(|note| note.contains("does not elaborate")));
}
