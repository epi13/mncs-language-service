//! Lineage fixtures as agent-facing service probes.
//!
//! These tests use the MNCS Lineage succession modules — realistic,
//! decision-dense MNCS Language artifacts — to verify that the resident
//! service answers the queries an autonomous agent needs when reasoning
//! about a lineage: clean diagnostics, symbol navigation across enum and
//! record vocabulary, reference finding on decision functions, and
//! obligations that preserve UNKNOWN instead of coercing it.

use mncs_service_core::LanguageService;
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

const G0: &str = "lineage/synthetic-lineage-g0.mncs";
const SELF_CERT: &str = "lineage/g0-proposer-self-certification.mncs";

fn open(svc: &LanguageService, name: &str) {
    let text = std::fs::read_to_string(fixtures_dir().join(name)).expect("fixture");
    svc.did_open(&fixture_uri(name), 1, text).expect("open");
}

#[test]
fn lineage_module_parses_cleanly_and_indexes_vocabulary() {
    let svc = service();
    open(&svc, G0);

    let diagnostics = svc
        .document_diagnostics(&fixture_uri(G0))
        .expect("diagnostics");
    assert!(
        diagnostics.items.is_empty(),
        "lineage module should be valid: {:?}",
        diagnostics.items
    );

    let symbols = svc.document_symbols(&fixture_uri(G0)).expect("symbols");
    let mut names: Vec<String> = Vec::new();
    let mut stack: Vec<&mncs_service_core::DocumentSymbolNode> = symbols.symbols.iter().collect();
    while let Some(node) = stack.pop() {
        names.push(node.summary.name.clone());
        for child in &node.children {
            stack.push(child);
        }
    }
    for expected in [
        "Verdict",
        "UnknownReason",
        "ClaimVerdict",
        "Disposition",
        "RollbackState",
        "CandidateSlot",
        "dependencies_unchanged",
        "effective_verdict",
        "candidate_disposition",
        "select_successor",
    ] {
        assert!(
            names.iter().any(|n| n.contains(expected)),
            "missing {expected} in {names:?}"
        );
    }
}

#[test]
fn hover_and_definition_survive_lineage_enums() {
    let svc = service();
    open(&svc, G0);
    let snapshot = svc.snapshot(&fixture_uri(G0)).expect("snapshot");
    let text = snapshot.text();

    // Locate `Verdict.UNKNOWN` inside `effective_verdict`'s downgrade return.
    let needle = "return Verdict.UNKNOWN;";
    let start = text.find(needle).expect("downgrade return present");
    let offset = start + needle.rfind("UNKNOWN").unwrap();
    let line = text[..offset].matches('\n').count() as u32;
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let character = (offset - line_start) as u32;

    let subjects = svc
        .subjects_at(&fixture_uri(G0), line, character)
        .expect("subjects");
    assert!(
        !subjects.occurrences.is_empty(),
        "no subject found at Verdict.UNKNOWN usage"
    );
    assert!(
        subjects
            .occurrences
            .iter()
            .any(|occurrence| occurrence.symbol.name == "UNKNOWN"),
        "expected the UNKNOWN variant as subject"
    );

    let definition = svc
        .definition(&fixture_uri(G0), line, character)
        .expect("definition");
    assert!(
        !definition.definitions.is_empty(),
        "variant usage should resolve to its declaration"
    );

    let hover = svc.hover(&fixture_uri(G0), line, character).expect("hover");
    assert!(hover.subject.is_some() || hover.markdown.is_some());
}

#[test]
fn references_resolve_on_decision_functions() {
    let svc = service();
    open(&svc, G0);
    let snapshot = svc.snapshot(&fixture_uri(G0)).expect("snapshot");
    let text = snapshot.text();

    // Declaration site of the selection policy function.
    let needle = "fn select_successor(";
    let offset = text.find(needle).expect("select_successor declared") + "fn ".len();
    let line = text[..offset].matches('\n').count() as u32;
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let character = (offset - line_start) as u32;

    let refs = svc
        .references(&fixture_uri(G0), line, character, true)
        .expect("references");
    assert!(
        !refs.hits.is_empty(),
        "declaration query must at least return the declaration itself"
    );
    assert!(refs.hits.iter().any(|hit| hit.is_declaration));
}

#[test]
fn self_certification_fixture_reports_elaboration_error_not_silence() {
    let svc = service();
    open(&svc, SELF_CERT);
    let diagnostics = svc
        .document_diagnostics(&fixture_uri(SELF_CERT))
        .expect("diagnostics");
    assert!(
        !diagnostics.items.is_empty(),
        "authority laundering must surface as an error to agents"
    );
    assert!(
        diagnostics
            .items
            .iter()
            .any(|item| item.code == "MNE134" && item.severity == "error"),
        "expected the authority-laundering diagnostic: {:?}",
        diagnostics.items
    );
}

#[test]
fn obligations_query_preserves_unknown_statuses_for_lineage_contracts() {
    let svc = service();
    open(&svc, G0);
    let obligations = svc
        .obligations(&fixture_uri(G0), None)
        .expect("obligations");
    assert!(!obligations.obligations.is_empty());
    // The raw source has no bound evidence yet, so contract-evidence
    // obligations are UNKNOWN; the service must report them verbatim instead
    // of coercing uncertainty into pass or failure.
    assert!(
        obligations
            .obligations
            .iter()
            .any(|o| o.status.eq_ignore_ascii_case("unknown")),
        "{:?}",
        obligations.obligations
    );
}
