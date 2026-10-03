//! Resident workspace-index tests: indexed cross-document queries must
//! observe exactly what per-document snapshot storms would find, must run no
//! compiler frontend work once warm, and must never serve stale entries
//! after edits.
//!
//! The differential test (`indexed_results_match_clean_rebuild`) is the
//! core trust anchor: an incrementally edited service must answer
//! identically to a fresh service built over the final files.

use mncs_service_core::{LanguageService, PositionMap, ResponseStatus};
use std::fs;
use std::path::{Path, PathBuf};

const BASE: &str = "mncs 0.6;\n\nmodule bench.base;\n\nenum Verdict { PASS, FAIL, UNKNOWN }\n\nfn demote(value: Verdict) -> (result: Verdict) {\n    return match value {\n        PASS => Verdict.UNKNOWN,\n        FAIL => Verdict.FAIL,\n        UNKNOWN => Verdict.UNKNOWN,\n    };\n}\n";
const MID: &str = "mncs 0.6;\n\nmodule bench.mid;\n\nuse bench.base;\n\nfn soften(value: Verdict) -> (result: Verdict) {\n    return demote(value);\n}\n";
const LEAF: &str = "mncs 0.6;\n\nmodule bench.leaf;\n\nuse bench.mid;\n\nfn polish(value: Verdict) -> (result: Verdict) {\n    return soften(value);\n}\n";
const TOP: &str = "mncs 0.6;\n\nmodule bench.top;\n\nuse bench.leaf;\nuse bench.mid;\nuse bench.base;\n\nfn finish(value: Verdict) -> (result: Verdict) {\n    let first: Verdict = polish(value);\n    let second: Verdict = demote(first);\n    return soften(second);\n}\n";

fn temp_workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mncs-language-service-wsindex-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary workspace");
    root
}

fn seed_chain(root: &Path) {
    fs::write(root.join("base.mncs"), BASE).expect("base");
    fs::write(root.join("mid.mncs"), MID).expect("mid");
    fs::write(root.join("leaf.mncs"), LEAF).expect("leaf");
    fs::write(root.join("top.mncs"), TOP).expect("top");
}

fn uri(root: &Path, name: &str) -> String {
    format!("file://{}", root.join(name).display())
}

/// (line, character) of the first occurrence of `needle` plus `offset`.
fn position_of(service: &LanguageService, uri: &str, needle: &str, offset: usize) -> (u32, u32) {
    let text = service.store().content(uri).expect("content");
    let byte = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle}"))
        + offset;
    let map = PositionMap::new(&text);
    let info = map.position_of(&text, byte);
    (info.line, info.character)
}

/// Normalize a response for cross-service comparison: snapshot provenance
/// (identities, generations) legitimately differs between an incrementally
/// edited service and a clean rebuild, so only status and payload compare.
fn normalized<T: serde::Serialize>(value: &T) -> serde_json::Value {
    let mut json = serde_json::to_value(value).expect("serializable");
    if let Some(object) = json.as_object_mut() {
        object.remove("snapshot");
        object.remove("generation");
    }
    json
}

fn base_identity(service: &LanguageService, base: &str) -> String {
    function_identity(service, base, "fn demote")
}

/// Module-qualified identity of the function declared by `decl_needle`.
fn function_identity(service: &LanguageService, uri: &str, decl_needle: &str) -> String {
    let (line, character) = position_of(service, uri, decl_needle, 3);
    let definitions = service
        .definition(uri, line, character)
        .expect("definition");
    assert_eq!(definitions.status, ResponseStatus::Answered);
    definitions.definitions[0]
        .identity
        .clone()
        .unwrap_or_else(|| panic!("identity for {decl_needle}"))
}

#[test]
fn warm_cross_document_queries_run_no_frontend_work() {
    let root = temp_workspace("warm");
    seed_chain(&root);
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("discover");
    let base = uri(&root, "base.mncs");
    let leaf = uri(&root, "leaf.mncs");
    let top = uri(&root, "top.mncs");

    // Warm every snapshot and index entry once.
    for name in ["base.mncs", "mid.mncs", "leaf.mncs", "top.mncs"] {
        service.snapshot(&uri(&root, name)).expect("warm snapshot");
    }
    service.reset_service_stats();

    let (decl_line, decl_character) = position_of(&service, &base, "fn demote", 3);
    let (call_line, call_character) = position_of(&service, &leaf, "soften(value)", 2);
    let identity = base_identity(&service, &base);

    let references = service
        .references(&base, decl_line, decl_character, true)
        .expect("references");
    assert_eq!(references.status, ResponseStatus::Answered);
    // Declaration plus one use site per importing module (mid, top).
    assert_eq!(references.hits.len(), 3, "{references:?}");

    let symbols = service.workspace_symbols("soften");
    assert_eq!(symbols.status, ResponseStatus::Answered);
    assert!(!symbols.symbols.is_empty());

    let definition = service
        .definition(&leaf, call_line, call_character)
        .expect("definition");
    assert_eq!(definition.status, ResponseStatus::Answered);

    let incoming = service.incoming_calls(&base, &identity).expect("incoming");
    assert_eq!(incoming.status, ResponseStatus::Answered);
    assert_eq!(incoming.edges.len(), 2, "{incoming:?}");

    let polish = function_identity(&service, &leaf, "fn polish");
    let outgoing = service.outgoing_calls(&leaf, &polish).expect("outgoing");
    assert_eq!(outgoing.status, ResponseStatus::Answered);

    let renamed = service
        .rename(&base, decl_line, decl_character, "demote_v2")
        .expect("rename");
    assert_eq!(renamed.status, ResponseStatus::Answered);
    assert_eq!(renamed.changes.len(), 3, "{renamed:?}");

    let stats = service.service_stats();
    assert_eq!(
        stats.frontend_runs, 0,
        "warm queries must not run the frontend"
    );
    assert_eq!(
        stats.snapshot_misses, 0,
        "warm queries must not miss snapshots"
    );
    assert!(stats.snapshot_hits > 0);
    assert_eq!(stats.indexed_documents, 4);
    assert!(stats.indexed_occurrences > 0);
    assert_eq!(stats.index_repairs, 0, "nothing was stale");
    let _ = top;
}

#[test]
fn indexed_results_match_clean_rebuild() {
    let root = temp_workspace("diff-a");
    seed_chain(&root);
    let incremental = LanguageService::new(Some(root.clone()));
    incremental.discover_workspace().expect("discover");
    let base = uri(&root, "base.mncs");
    let mid = uri(&root, "mid.mncs");
    let leaf = uri(&root, "leaf.mncs");
    let top = uri(&root, "top.mncs");
    for uri in [&base, &mid, &leaf, &top] {
        incremental.snapshot(uri).expect("warm");
    }

    // A body edit (semantics preserved), then a comment-only edit.
    let original = (*incremental.store().content(&base).expect("base text")).clone();
    let edited = original.replace(
        "PASS => Verdict.UNKNOWN,",
        "PASS => Verdict.UNKNOWN, // clarified",
    );
    incremental.did_open(&base, 2, edited).expect("body edit");
    let current = (*incremental.store().content(&mid).expect("mid text")).clone();
    incremental
        .did_open(&mid, 2, format!("{current}// touched\n"))
        .expect("comment edit");

    // Clean rebuild over the final files.
    let clean_root = temp_workspace("diff-b");
    for (name, uri) in [
        ("base.mncs", &base),
        ("mid.mncs", &mid),
        ("leaf.mncs", &leaf),
        ("top.mncs", &top),
    ] {
        let text = incremental.store().content(uri).expect("final text");
        fs::write(clean_root.join(name), text.as_str()).expect("copy");
    }
    let clean = LanguageService::new(Some(clean_root.clone()));
    clean.discover_workspace().expect("discover");
    let clean_base = uri(&clean_root, "base.mncs");
    let clean_mid = uri(&clean_root, "mid.mncs");
    let clean_leaf = uri(&clean_root, "leaf.mncs");
    let clean_top = uri(&clean_root, "top.mncs");

    // Same positions in both trees (edits preserved line layout).
    let (decl_line, decl_character) = position_of(&incremental, &base, "fn demote", 3);
    let (call_line, call_character) = position_of(&incremental, &leaf, "soften(value)", 2);
    let identity = base_identity(&incremental, &base);

    let pairs: Vec<(&str, serde_json::Value, serde_json::Value)> = vec![
        (
            "references",
            normalized(
                &incremental
                    .references(&base, decl_line, decl_character, true)
                    .expect("references"),
            ),
            normalized(
                &clean
                    .references(&clean_base, decl_line, decl_character, true)
                    .expect("references"),
            ),
        ),
        (
            "workspace_symbols",
            normalized(&incremental.workspace_symbols("en")),
            normalized(&clean.workspace_symbols("en")),
        ),
        (
            "definition",
            normalized(
                &incremental
                    .definition(&leaf, call_line, call_character)
                    .expect("definition"),
            ),
            normalized(
                &clean
                    .definition(&clean_leaf, call_line, call_character)
                    .expect("definition"),
            ),
        ),
        (
            "incoming_calls",
            normalized(
                &incremental
                    .incoming_calls(&base, &identity)
                    .expect("incoming"),
            ),
            normalized(
                &clean
                    .incoming_calls(&clean_base, &identity)
                    .expect("incoming"),
            ),
        ),
        (
            "outgoing_calls",
            normalized(
                &incremental
                    .outgoing_calls(&leaf, &function_identity(&incremental, &leaf, "fn polish"))
                    .expect("outgoing"),
            ),
            normalized(
                &clean
                    .outgoing_calls(
                        &clean_leaf,
                        &function_identity(&clean, &clean_leaf, "fn polish"),
                    )
                    .expect("outgoing"),
            ),
        ),
        (
            "rename",
            normalized(
                &incremental
                    .rename(&base, decl_line, decl_character, "demote_v2")
                    .expect("rename"),
            ),
            normalized(
                &clean
                    .rename(&clean_base, decl_line, decl_character, "demote_v2")
                    .expect("rename"),
            ),
        ),
        (
            "type_definition",
            normalized(
                &incremental
                    .type_definition(&leaf, call_line, call_character)
                    .expect("type definition"),
            ),
            normalized(
                &clean
                    .type_definition(&clean_leaf, call_line, call_character)
                    .expect("type definition"),
            ),
        ),
        (
            "transitive_dependents",
            normalized(
                &incremental
                    .transitive_dependents(&base, 8)
                    .expect("dependents"),
            ),
            normalized(
                &clean
                    .transitive_dependents(&clean_base, 8)
                    .expect("dependents"),
            ),
        ),
        (
            "transitive_dependencies",
            normalized(
                &incremental
                    .transitive_dependencies(&top, 8)
                    .expect("dependencies"),
            ),
            normalized(
                &clean
                    .transitive_dependencies(&clean_top, 8)
                    .expect("dependencies"),
            ),
        ),
        (
            "transitive_callers",
            normalized(
                &incremental
                    .transitive_callers(&identity, 8)
                    .expect("callers"),
            ),
            normalized(&clean.transitive_callers(&identity, 8).expect("callers")),
        ),
    ];
    // URIs embed the temp roots; compare with roots masked.
    for (name, mut left, mut right) in pairs {
        mask_roots(&mut left, &root, &clean_root);
        mask_roots(&mut right, &root, &clean_root);
        assert_eq!(left, right, "incremental vs clean diverged for {name}");
    }
    let _ = clean_mid;
}

/// Replace both temp-root prefixes with a placeholder so payloads compare.
fn mask_roots(value: &mut serde_json::Value, left: &Path, right: &Path) {
    match value {
        serde_json::Value::String(text) => {
            for root in [left, right] {
                let prefix = format!("file://{}", root.display());
                if text.starts_with(&prefix) {
                    *text = text.replacen(&prefix, "file://ROOT", 1);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_roots(item, left, right);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                mask_roots(value, left, right);
            }
        }
        _ => {}
    }
}

#[test]
fn dependency_edit_repairs_exactly_the_affected_entries() {
    let root = temp_workspace("depedit");
    seed_chain(&root);
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("discover");
    let base = uri(&root, "base.mncs");
    let mid = uri(&root, "mid.mncs");
    let leaf = uri(&root, "leaf.mncs");
    let top = uri(&root, "top.mncs");
    for uri in [&base, &mid, &leaf, &top] {
        service.snapshot(uri).expect("warm");
    }
    service.reset_service_stats();

    let original = (*service.store().content(&base).expect("base text")).clone();
    let edited = original.replace("FAIL => Verdict.FAIL,", "FAIL => Verdict.UNKNOWN,");
    assert_ne!(edited, original);
    service.did_open(&base, 2, edited).expect("edit");

    let (decl_line, decl_character) = position_of(&service, &base, "fn demote", 3);
    let references = service
        .references(&base, decl_line, decl_character, true)
        .expect("references");
    assert_eq!(references.status, ResponseStatus::Answered);
    assert_eq!(references.hits.len(), 3, "{references:?}");

    // Exactly one frontend run per affected document: the edited base plus
    // its three transitive importers (direct validation alone would miss
    // the transitive ones and serve stale elaborations). Nothing else
    // re-analyzes.
    let stats = service.service_stats();
    assert_eq!(stats.frontend_runs, 4, "{stats:?}");
    assert_eq!(stats.index_repairs, 3, "{stats:?}");
    assert_eq!(stats.indexed_documents, 4);
}

#[test]
fn removed_symbols_resolve_to_unresolved_not_stale_summaries() {
    let root = temp_workspace("stale");
    seed_chain(&root);
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("discover");
    let mid = uri(&root, "mid.mncs");
    let leaf = uri(&root, "leaf.mncs");

    let (call_line, call_character) = position_of(&service, &leaf, "soften(value)", 2);
    let before = service
        .definition(&leaf, call_line, call_character)
        .expect("definition");
    assert_eq!(before.status, ResponseStatus::Answered);

    // Remove `soften` from mid; leaf still calls it (now unresolvable).
    let broken = "mncs 0.6;\n\nmodule bench.mid;\n\nuse bench.base;\n\nfn helper(value: Verdict) -> (result: Verdict) {\n    return demote(value);\n}\n";
    service.did_open(&mid, 2, broken.to_owned()).expect("edit");

    let after = service
        .definition(&leaf, call_line, call_character)
        .expect("definition");
    assert_eq!(
        after.status,
        ResponseStatus::Unresolved {
            reason: "position does not resolve to a declaration".to_owned(),
        },
        "{after:?}"
    );
    let symbols = service.workspace_symbols("soften");
    assert!(
        !symbols
            .symbols
            .iter()
            .any(|hit| hit.summary.name == "soften"),
        "removed symbol must vanish from the index: {symbols:?}"
    );
}

#[test]
fn transitive_walks_report_depths_and_stay_complete() {
    let root = temp_workspace("transitive");
    seed_chain(&root);
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("discover");
    let base = uri(&root, "base.mncs");
    let mid = uri(&root, "mid.mncs");
    let leaf = uri(&root, "leaf.mncs");
    let top = uri(&root, "top.mncs");

    let dependents = service.transitive_dependents(&base, 8).expect("dependents");
    assert_eq!(dependents.status, ResponseStatus::Answered);
    assert!(dependents.complete);
    let depths: Vec<(&str, usize)> = dependents
        .nodes
        .iter()
        .map(|node| {
            let name = node.uri.rsplit('/').next().unwrap_or(&node.uri);
            (name, node.depth)
        })
        .collect();
    assert_eq!(
        depths,
        vec![("mid.mncs", 1), ("top.mncs", 1), ("leaf.mncs", 2)],
        "{dependents:?}"
    );

    let dependencies = service
        .transitive_dependencies(&top, 8)
        .expect("dependencies");
    assert_eq!(dependencies.status, ResponseStatus::Answered);
    assert!(dependencies.complete);
    let depths: Vec<(&str, usize)> = dependencies
        .nodes
        .iter()
        .map(|node| {
            let name = node.uri.rsplit('/').next().unwrap_or(&node.uri);
            (name, node.depth)
        })
        .collect();
    assert_eq!(
        depths,
        vec![("base.mncs", 1), ("leaf.mncs", 1), ("mid.mncs", 1)],
        "{dependencies:?}"
    );

    let identity = base_identity(&service, &base);
    let callers = service.transitive_callers(&identity, 8).expect("callers");
    assert_eq!(callers.status, ResponseStatus::Answered);
    assert!(callers.complete);
    let names: Vec<(&str, usize)> = callers
        .nodes
        .iter()
        .map(|node| (node.name.as_str(), node.depth))
        .collect();
    // soften and finish call demote directly; polish calls soften.
    assert!(names.contains(&("soften", 1)), "{callers:?}");
    assert!(names.contains(&("finish", 1)), "{callers:?}");
    assert!(names.contains(&("polish", 2)), "{callers:?}");

    // Depth bound truncates honestly.
    let shallow = service.transitive_dependents(&base, 1).expect("shallow");
    assert_eq!(shallow.nodes.len(), 2);
    assert!(shallow.nodes.iter().all(|node| node.depth == 1));

    // Unknown subjects stay explicit.
    let missing = service
        .transitive_callers("mncs:0.2:function:bench.base::missing", 8)
        .expect("missing");
    assert!(matches!(missing.status, ResponseStatus::Unresolved { .. }));
    let _ = (mid, leaf);
}

#[test]
fn concurrent_indexed_queries_agree() {
    use std::sync::Arc;

    let root = temp_workspace("concurrent");
    seed_chain(&root);
    let service = Arc::new(LanguageService::new(Some(root.clone())));
    service.discover_workspace().expect("discover");
    let base = uri(&root, "base.mncs");
    let (decl_line, decl_character) = position_of(&service, &base, "fn demote", 3);
    let expected = normalized(
        &service
            .references(&base, decl_line, decl_character, true)
            .expect("references"),
    );

    let mut handles = Vec::new();
    for _ in 0..8 {
        let service = Arc::clone(&service);
        let base = base.clone();
        let expected = expected.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..10 {
                let references = service
                    .references(&base, decl_line, decl_character, true)
                    .expect("references");
                assert_eq!(normalized(&references), expected);
                let symbols = service.workspace_symbols("en");
                assert!(!symbols.symbols.is_empty());
                let dependents = service.transitive_dependents(&base, 8).expect("dependents");
                assert_eq!(dependents.nodes.len(), 3, "{dependents:?}");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker did not panic");
    }
}
