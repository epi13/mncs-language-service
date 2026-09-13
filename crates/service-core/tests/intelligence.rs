//! Integration tests for second-wave semantic intelligence: signature help,
//! declaration/type definition, selection ranges, call hierarchy, inlay
//! hints, semantic rename, formatting, and import-assist code actions.

use mncs_service_core::{LanguageService, ResponseStatus};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn fixture_uri(name: &str) -> String {
    format!("file://{}", fixtures_dir().join(name).display())
}

fn service() -> LanguageService {
    LanguageService::new(Some(fixtures_dir()))
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).expect("fixture")
}

/// LSP-style position of `needle` plus a character delta, using the same
/// coordinate layer as the service.
fn position_of(text: &str, needle: &str, plus: usize) -> (u32, u32) {
    let map = mncs_service_core::PositionMap::new(text);
    let start = text.find(needle).expect("needle present");
    let info = map.position_of(text, start + plus);
    (info.line, info.character)
}

// ---------------------------------------------------------------------------
// Signature help
// ---------------------------------------------------------------------------

#[test]
fn signature_help_reports_callee_and_active_argument() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");

    // Call site: `return bounded_step(value, value);` — cursor on 2nd arg.
    let call = text.rfind("bounded_step(value, value)").expect("call");
    let second_arg = call + "bounded_step(value, ".len();
    let map = mncs_service_core::PositionMap::new(&text);
    let info = map.position_of(&text, second_arg);
    let response = svc
        .signature_help(&uri, info.line, info.character)
        .expect("signature help");
    assert!(matches!(response.status, ResponseStatus::Answered));
    let label = response.label.expect("label");
    assert!(
        label.contains("fn bounded_step(n: i64, limit: i64) -> (result: i64)"),
        "{label}"
    );
    assert_eq!(response.active_parameter, 1);
    assert_eq!(response.parameters.len(), 2);
    assert_eq!(response.parameters[0].name, "n");
    assert_eq!(response.parameters[1].name, "limit");
    assert_eq!(response.return_type.as_deref(), Some("i64"));

    // First argument → active 0.
    let first_arg = call + "bounded_step(".len();
    let info = map.position_of(&text, first_arg);
    let response = svc
        .signature_help(&uri, info.line, info.character)
        .expect("signature help");
    assert_eq!(response.active_parameter, 0);
}

#[test]
fn signature_help_survives_unclosed_calls_mid_typing() {
    // The edited document no longer parses, but the callee declaration lives
    // in a stable workspace document — the realistic mid-typing state.
    let svc = LanguageService::new(None);
    svc.did_open(
        "untitled:sig-stable",
        1,
        "mncs 0.3;\n\nmodule stable.callee;\n\nfn callee(a: i64, b: i64) -> (result: i64)\n{\n    return a;\n}\n".to_owned(),
    )
    .expect("open stable");
    let uri = "untitled:sig-midtyping";
    let text = "mncs 0.3;\n\nmodule mid.typing;\n\nfn caller(v: i64) -> (result: i64)\n{\n    return callee(v, ";
    svc.did_open(uri, 1, text.to_owned()).expect("open");
    // Cursor at end of buffer, inside the still-open argument list.
    let map = mncs_service_core::PositionMap::new(text);
    let info = map.position_of(text, text.len());
    let response = svc
        .signature_help(uri, info.line, info.character)
        .expect("signature help");
    assert!(
        matches!(response.status, ResponseStatus::Answered),
        "{:?}",
        response.status
    );
    assert_eq!(response.active_parameter, 1);
    assert!(response.label.expect("label").contains("fn callee"));
}

#[test]
fn signature_help_is_unresolved_outside_calls() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    let (line, character) = position_of(&text, "module examples.contracts", 2);
    let response = svc
        .signature_help(&uri, line, character)
        .expect("signature help");
    assert!(matches!(response.status, ResponseStatus::Unresolved { .. }));
}

// ---------------------------------------------------------------------------
// Declaration / type definition
// ---------------------------------------------------------------------------

#[test]
fn declaration_matches_definition_for_single_site_language() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    let call = text.rfind("bounded_step").expect("call site");
    let map = mncs_service_core::PositionMap::new(&text);
    let info = map.position_of(&text, call);
    let definition = svc
        .definition(&uri, info.line, info.character)
        .expect("definition");
    let declaration = svc
        .declaration(&uri, info.line, info.character)
        .expect("declaration");
    assert!(matches!(definition.status, ResponseStatus::Answered));
    assert_eq!(declaration.definitions, definition.definitions);
}

#[test]
fn type_definition_resolves_declared_types_and_rejects_builtins() {
    let svc = service();
    let uri = fixture_uri("records.mncs");
    let text = fixture_text("records.mncs");

    // `base` is `let base: Reading` → the record declaration.
    let map = mncs_service_core::PositionMap::new(&text);
    let base_decl = text.find("let base: Reading").expect("decl") + 4;
    let info = map.position_of(&text, base_decl);
    let response = svc
        .type_definition(&uri, info.line, info.character)
        .expect("type definition");
    assert!(
        matches!(response.status, ResponseStatus::Answered),
        "{:?}",
        response.status
    );
    assert_eq!(response.definitions.len(), 1);
    assert_eq!(response.definitions[0].name, "Reading");

    // `celsius: i32` → builtin scalars have no declaration site.
    let param = text.find("celsius: i32").expect("param");
    let info = map.position_of(&text, param);
    let response = svc
        .type_definition(&uri, info.line, info.character)
        .expect("type definition");
    assert!(
        matches!(response.status, ResponseStatus::Unresolved { .. }),
        "{:?}",
        response.status
    );
}

// ---------------------------------------------------------------------------
// Selection ranges
// ---------------------------------------------------------------------------

#[test]
fn selection_ranges_nest_from_innermost_to_document() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    let at = text.find("return next;").expect("stmt") + "return ".len();
    let map = mncs_service_core::PositionMap::new(&text);
    let info = map.position_of(&text, at);
    let response = svc
        .selection_ranges(&uri, &[(info.line, info.character)])
        .expect("selection ranges");
    assert!(matches!(response.status, ResponseStatus::Answered));
    assert_eq!(response.chains.len(), 1);
    let chain = &response.chains[0].ranges;
    assert!(chain.len() >= 3, "expected nesting, got {chain:?}");
    // Innermost-first containment.
    for window in chain.windows(2) {
        assert!(
            window[0].start_byte >= window[1].start_byte
                && window[0].end_byte <= window[1].end_byte,
            "{window:?}"
        );
    }
    // Outermost covers the whole document.
    let outer = chain.last().expect("outer");
    assert_eq!(outer.start_byte, 0);
    assert_eq!(outer.end_byte, text.len());
}

// ---------------------------------------------------------------------------
// Call hierarchy
// ---------------------------------------------------------------------------

#[test]
fn call_hierarchy_links_callers_and_callees() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    let (line, character) = position_of(&text, "fn caller", 3);
    let prepared = svc
        .prepare_call_hierarchy(&uri, line, character)
        .expect("prepare");
    assert!(matches!(prepared.status, ResponseStatus::Answered));
    assert_eq!(prepared.items.len(), 1);
    assert_eq!(prepared.items[0].name, "caller");
    let caller_id = prepared.items[0].identity.clone().expect("caller identity");

    let outgoing = svc.outgoing_calls(&uri, &caller_id).expect("outgoing");
    assert!(matches!(outgoing.status, ResponseStatus::Answered));
    assert_eq!(outgoing.edges.len(), 1);
    assert_eq!(outgoing.edges[0].item.name, "bounded_step");
    assert!(
        !outgoing.edges[0].from_ranges.is_empty(),
        "call-site ranges required"
    );

    let callee_id = outgoing.edges[0]
        .item
        .identity
        .clone()
        .expect("callee identity");
    let incoming = svc.incoming_calls(&uri, &callee_id).expect("incoming");
    assert!(matches!(incoming.status, ResponseStatus::Answered));
    assert_eq!(incoming.edges.len(), 1);
    assert_eq!(incoming.edges[0].item.name, "caller");
    assert!(!incoming.edges[0].from_ranges.is_empty());
}

#[test]
fn prepare_call_hierarchy_rejects_non_functions() {
    let svc = service();
    let uri = fixture_uri("records.mncs");
    let text = fixture_text("records.mncs");
    // Module name position: hierarchies prepare only for functions.
    let (line, character) = position_of(&text, "module examples.records", 2);
    let response = svc
        .prepare_call_hierarchy(&uri, line, character)
        .expect("prepare");
    assert!(
        matches!(response.status, ResponseStatus::Unresolved { .. }),
        "{:?}",
        response.status
    );
}

// ---------------------------------------------------------------------------
// Inlay hints
// ---------------------------------------------------------------------------

#[test]
fn inlay_hints_label_call_arguments_with_parameter_names() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let hints = svc.inlay_hints(&uri, 0, 0, 1000, 0).expect("inlay hints");
    assert!(matches!(hints.status, ResponseStatus::Answered));
    let labels: Vec<&str> = hints.hints.iter().map(|hint| hint.label.as_str()).collect();
    assert!(
        labels.contains(&"n:") && labels.contains(&"limit:"),
        "{labels:?}"
    );
    // Hints sit at argument starts on the call line.
    let text = fixture_text("valid-contracts.mncs");
    let call_line = text
        .lines()
        .position(|line| line.contains("bounded_step(value, value)"))
        .expect("call line") as u32;
    assert!(hints.hints.iter().all(|hint| hint.line == call_line));
}

// ---------------------------------------------------------------------------
// Semantic rename
// ---------------------------------------------------------------------------

fn apply_rename(
    svc: &LanguageService,
    uri: &str,
    needle: &str,
    plus: usize,
    new_name: &str,
) -> mncs_service_core::RenameResponse {
    let text = svc.store().content(uri).expect("content");
    let text = (*text).clone();
    let (line, character) = position_of(&text, needle, plus);
    svc.rename(uri, line, character, new_name).expect("rename")
}

#[test]
fn rename_collects_declaration_and_bound_references_only() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    svc.did_open(&uri, 1, text).expect("open");

    // Local binding `next`: declaration + its single use.
    let response = apply_rename(&svc, &uri, "let next: i64", 4, "following");
    assert!(
        matches!(response.status, ResponseStatus::Answered),
        "{:?}",
        response.status
    );
    assert_eq!(response.changes.len(), 1);
    assert_eq!(response.changes[0].edits.len(), 2);
    assert!(response.changes[0]
        .edits
        .iter()
        .all(|edit| edit.new_text == "following"));

    // Function rename reaches the call site.
    let response = apply_rename(&svc, &uri, "fn bounded_step", 3, "bounded_stride");
    assert!(matches!(response.status, ResponseStatus::Answered));
    assert_eq!(response.changes.len(), 1);
    assert_eq!(response.changes[0].edits.len(), 2, "decl + call");
}

#[test]
fn rename_rejects_collisions_keywords_and_modules() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    svc.did_open(&uri, 1, text).expect("open");

    // `n` is a parameter of the same function: collision.
    let response = apply_rename(&svc, &uri, "let next: i64", 4, "n");
    assert!(
        matches!(response.status, ResponseStatus::Unresolved { .. }),
        "{:?}",
        response.status
    );
    assert!(response.changes.is_empty());

    // Reserved keyword.
    let response = apply_rename(&svc, &uri, "let next: i64", 4, "fn");
    assert!(matches!(response.status, ResponseStatus::Unresolved { .. }));

    // Module rename is out of scope (file + import graph move).
    let response = apply_rename(&svc, &uri, "module examples.contracts", 7, "other");
    assert!(matches!(response.status, ResponseStatus::Unresolved { .. }));
}

#[test]
fn rename_applies_cleanly_and_reanalyzes() {
    let svc = service();
    let uri = fixture_uri("valid-contracts.mncs");
    let text = fixture_text("valid-contracts.mncs");
    svc.did_open(&uri, 1, text.clone()).expect("open");

    let response = apply_rename(&svc, &uri, "fn bounded_step", 3, "bounded_stride");
    assert!(matches!(response.status, ResponseStatus::Answered));

    // Materialize the workspace edit exactly as an LSP client would: sort
    // edits back-to-front per file and splice.
    let mut current = text;
    for file in &response.changes {
        assert_eq!(file.uri, uri);
        let mut edits = file.edits.clone();
        edits.sort_by_key(|edit| edit.range.start_byte);
        for edit in edits.into_iter().rev() {
            current.replace_range(edit.range.start_byte..edit.range.end_byte, &edit.new_text);
        }
    }
    assert!(current.contains("fn bounded_stride"));
    assert!(!current.contains("bounded_step"));
    svc.did_change(&uri, 2, current).expect("change");
    let diagnostics = svc.document_diagnostics(&uri).expect("diagnostics");
    assert!(
        diagnostics.items.is_empty(),
        "renamed program stays valid: {:#?}",
        diagnostics.items
    );
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

#[test]
fn formatting_is_stable_and_idempotent_on_fixtures() {
    let svc = service();
    for name in ["valid-contracts.mncs", "records.mncs", "finite-match.mncs"] {
        let uri = fixture_uri(name);
        let first = svc.formatting(&uri).expect("formatting");
        assert!(matches!(first.status, ResponseStatus::Answered));
        // Canonical fixtures stay canonical.
        assert!(first.already_formatted, "{name} should be canonical");
        // Messy input converges and then stays fixed.
        let messy = first
            .text
            .replace("    ", "        ")
            .replace("\n\n", "\n\n\n");
        let uri_messy = format!("untitled:messy-{name}");
        svc.did_open(&uri_messy, 1, messy).expect("open");
        let once = svc.formatting(&uri_messy).expect("format once");
        assert!(!once.already_formatted);
        svc.did_change(&uri_messy, 2, once.text.clone())
            .expect("apply");
        let twice = svc.formatting(&uri_messy).expect("format twice");
        assert!(twice.already_formatted);
        assert_eq!(twice.text, once.text);
    }
}

#[test]
fn range_formatting_scopes_edits_to_the_range() {
    let svc = LanguageService::new(None);
    let uri = "untitled:range-fmt";
    let text = "mncs 0.3;\nmodule m;\nfn f(n: i64) -> (result: i64)\n{\n        return n;\n}\n";
    svc.did_open(uri, 1, text.to_owned()).expect("open");
    // Only the body line is in range; the (already fine) header stays out.
    let response = svc.range_formatting(uri, 4, 4).expect("range format");
    assert!(matches!(response.status, ResponseStatus::Answered));
    assert!(!response.already_formatted);
    assert_eq!(response.changes.len(), 1);
    assert!(response.changes[0]
        .edits
        .iter()
        .all(|edit| edit.range.start_line == 4));
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

#[test]
fn missing_import_quickfix_offers_use_insertion() {
    let svc = LanguageService::new(None);
    let exporter = "untitled:exporter";
    svc.did_open(
        exporter,
        1,
        "mncs 0.3;\n\nmodule helpers.math;\n\nfn double(n: i64) -> (result: i64)\n{\n    return n + n;\n}\n".to_owned(),
    )
    .expect("open exporter");
    let importer = "untitled:importer";
    let importing = "mncs 0.3;\n\nmodule app.main;\n\nfn run(v: i64) -> (result: i64)\n{\n    return double(v);\n}\n";
    svc.did_open(importer, 1, importing.to_owned())
        .expect("open");

    let diagnostics = svc.document_diagnostics(importer).expect("diagnostics");
    let unresolved = diagnostics
        .items
        .iter()
        .find(|item| item.code == "MNE131")
        .expect("unresolved call diagnostic");
    let actions = svc
        .code_actions(
            importer,
            unresolved.range.start_line,
            unresolved.range.start_character,
            unresolved.range.end_line,
            unresolved.range.end_character,
        )
        .expect("code actions");
    assert!(
        actions
            .actions
            .iter()
            .any(|action| action.title.contains("use helpers.math;")),
        "{:#?}",
        actions.actions
    );
    // The offered edit inserts exactly one import line after the module decl.
    let action = actions
        .actions
        .iter()
        .find(|action| action.title.contains("helpers.math"))
        .expect("import action");
    let edit = action.edit.as_ref().expect("edit");
    assert_eq!(edit.edits.len(), 1);
    assert_eq!(edit.edits[0].new_text, "use helpers.math;\n");
}
