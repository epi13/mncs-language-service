//! Cross-module analysis: `use` imports resolve against resident documents,
//! diagnostics flow across the boundary, and editing a dependency invalidates
//! dependent snapshots.

use mncs_service_core::LanguageService;
use std::path::PathBuf;

fn imports_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/imports")
}

fn uri(name: &str) -> String {
    format!("file://{}", imports_dir().join(name).display())
}

#[test]
fn imported_modules_link_and_report_diagnostics_across_documents() {
    let svc = LanguageService::new(Some(imports_dir()));
    svc.discover_workspace().expect("discovery");

    let study = svc.snapshot(&uri("study.mncs")).expect("snapshot");
    assert!(
        study.valid(),
        "importer must elaborate cleanly: {:?}",
        study
            .front_end
            .diagnostics
            .iter()
            .map(|d| d.code.clone())
            .collect::<Vec<_>>()
    );

    // The linked program contains both modules' functions.
    let program = study.front_end.program.as_ref().expect("program");
    let mut names = program
        .functions
        .iter()
        .map(|f| f.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["demote".to_owned(), "soften".to_owned()]);
    assert_eq!(program.dependencies.len(), 1);

    // Editing a dependency invalidates the dependent's cached snapshot even
    // though its own text is unchanged.
    let before = ArcSnapshot(svc.snapshot(&uri("study.mncs")).expect("before"));
    let evidence_uri = uri("evidence.mncs");
    let current = svc.snapshot(&evidence_uri).expect("dependency snapshot");
    let edited = current
        .text()
        .replace("PASS => Verdict.UNKNOWN,", "PASS => Verdict.FAIL,\n");
    svc.did_open(&evidence_uri, 2, edited).expect("open buffer");

    let after = svc.snapshot(&uri("study.mncs")).expect("after");
    assert!(
        !ArcSnapshot::ptr_eq(&before, &ArcSnapshot(after)),
        "dependent snapshot must be reanalyzed after a dependency edit"
    );
}

#[test]
fn imported_symbols_navigate_to_the_declaring_document() {
    let svc = LanguageService::new(Some(imports_dir()));
    svc.discover_workspace().expect("discovery");
    let study_uri = uri("study.mncs");
    let study = svc.snapshot(&study_uri).expect("study snapshot");
    let offset = study.text().find("demote").expect("imported call");
    let position = study.positions.position_of(study.text(), offset);

    let definitions = svc
        .definition(&study_uri, position.line, position.character)
        .expect("definition");
    assert_eq!(definitions.definitions.len(), 1);
    assert!(definitions.definitions[0]
        .uri
        .as_deref()
        .is_some_and(|uri| uri.ends_with("evidence.mncs")));

    let hover = svc
        .hover(&study_uri, position.line, position.character)
        .expect("hover");
    assert!(hover
        .markdown
        .as_deref()
        .is_some_and(|markdown| markdown.contains("demote")));

    let references = svc
        .references(&study_uri, position.line, position.character, true)
        .expect("references");
    assert!(references
        .hits
        .iter()
        .any(|hit| hit.is_declaration && hit.uri.ends_with("evidence.mncs")));
    assert!(references
        .hits
        .iter()
        .any(|hit| !hit.is_declaration && hit.uri.ends_with("study.mncs")));
}

struct ArcSnapshot(std::sync::Arc<mncs_service_core::DocumentAnalysis>);

impl ArcSnapshot {
    fn ptr_eq(a: &ArcSnapshot, b: &ArcSnapshot) -> bool {
        std::sync::Arc::ptr_eq(&a.0, &b.0)
    }
}
