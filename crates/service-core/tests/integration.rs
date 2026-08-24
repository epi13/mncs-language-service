//! Integration tests exercising the resident core as a language service:
//! snapshots, invalidation, navigation, diagnostics, obligations, tokens,
//! completion, and failure behavior against representative MNCS fixtures.

use mncs_service_core::{LanguageService, ResponseStatus, SymbolKind};
use std::path::PathBuf;
use std::sync::Arc;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
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
    let status = svc.workspace_status();
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
