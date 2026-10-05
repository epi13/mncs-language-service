//! Standard-library discovery: an explicit `MNCS_STDLIB_ROOT` binds `use`
//! imports to a checkout's `library/` tree, `use`-line completion proposes
//! manifest module paths, and an empty root disables discovery for hermetic
//! sessions. One sequential test: the discovery inputs are process-global.
//!
//! The fixture tree lives at `tests/fixtures/stdlib/` with a minimal manifest
//! (`mncs.demo.widget.v1`) and its module source.

use mncs_service_core::LanguageService;
use std::path::PathBuf;

fn stdlib_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/stdlib")
}

const CONSUMER: &str = "untitled:stdlib-consumer";

fn consumer_text() -> String {
    "mncs 0.6;\n\nmodule fixtures.stdlib.consumer;\n\nuse mncs.demo.widget.v1;\n\nfn run(value: Widget) -> (result: Widget) {\n    return identify(value);\n}\n".to_owned()
}

#[test]
fn stdlib_discovery_resolution_and_completion() {
    // Phase 1: an explicit checkout root resolves library imports without
    // any MNCS_LIBRARY_PATH entry.
    std::env::remove_var("MNCS_LIBRARY_PATH");
    std::env::set_var("MNCS_STDLIB_ROOT", stdlib_fixture());

    let svc = LanguageService::new(None);
    svc.did_open(CONSUMER, 1, consumer_text())
        .expect("open consumer");
    let study = svc.snapshot(CONSUMER).expect("snapshot");
    assert!(
        study.valid(),
        "consumer must elaborate against the stdlib checkout: {:?}",
        study
            .front_end
            .diagnostics
            .iter()
            .map(|d| d.code.clone())
            .collect::<Vec<_>>()
    );
    let program = study.front_end.program.as_ref().expect("program");
    let mut names = program
        .functions
        .iter()
        .map(|f| f.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["identify".to_owned(), "run".to_owned()]);

    // Phase 2: `use`-line completion proposes the manifest module path.
    let text = consumer_text();
    let map = mncs_service_core::PositionMap::new(&text);
    let offset = text.find("use mncs.demo.w").expect("needle") + "use mncs.demo.w".len();
    let position = map.position_of(&text, offset);
    let items = svc
        .completion(CONSUMER, position.line, position.character)
        .expect("completion");
    assert!(
        items.items.iter().any(|candidate| {
            candidate.label == "mncs.demo.widget.v1"
                && candidate.class == mncs_service_core::CompletionClass::Module
        }),
        "manifest module proposed on the use line: {:#?}",
        items.items
    );

    // Phase 3: an empty root disables discovery; the manifest contributes
    // no names and the consumer no longer resolves.
    std::env::set_var("MNCS_STDLIB_ROOT", "");
    let bare = LanguageService::new(None);
    bare.did_open(CONSUMER, 1, consumer_text())
        .expect("open consumer");
    let missing = bare.snapshot(CONSUMER).expect("snapshot");
    assert!(
        !missing.valid(),
        "consumer must fail closed with discovery disabled"
    );
    let items = bare
        .completion(CONSUMER, position.line, position.character)
        .expect("completion");
    assert!(
        items
            .items
            .iter()
            .all(|candidate| { candidate.class != mncs_service_core::CompletionClass::Module }),
        "no manifest modules offered with discovery disabled: {:#?}",
        items.items
    );
}
