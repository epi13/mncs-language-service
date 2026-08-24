//! Performance sanity: the service must amortize analysis across repeated
//! queries, must not hold locks during frontend work, and must stay fast
//! enough for editor-round-trip use on representative files.

use mncs_service_core::LanguageService;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn uri_for(name: &str) -> String {
    format!(
        "file://{}",
        fixtures_dir()
            .join(name)
            .canonicalize()
            .expect("fixture path")
            .display()
    )
}

#[test]
fn repeated_queries_reuse_snapshots_and_stay_fast() {
    let svc = LanguageService::new(Some(fixtures_dir()));
    let uri = uri_for("valid-contracts.mncs");

    // Warm-up (includes the one authoritative frontend run).
    let start = Instant::now();
    svc.snapshot(&uri).expect("first snapshot");
    let first = start.elapsed();

    let start = Instant::now();
    for _ in 0..100 {
        let _ = svc.hover(&uri, 4, 10).expect("hover");
        let _ = svc.document_symbols(&uri).expect("symbols");
        let _ = svc.obligations(&uri, None).expect("obligations");
    }
    let repeated = start.elapsed();

    assert!(
        repeated < Duration::from_secs(5),
        "300 queries took {repeated:?}; snapshot reuse is broken"
    );
    println!("first analysis: {first:?}; 300 queries on resident snapshots: {repeated:?}");
}

#[test]
fn concurrent_queries_do_not_deadlock_or_duplicate_analysis() {
    let svc = Arc::new(LanguageService::new(Some(fixtures_dir())));
    let uris: Vec<String> = ["valid-contracts.mncs", "finite-match.mncs", "records.mncs"]
        .iter()
        .map(|name| uri_for(name))
        .collect();

    let mut handles = Vec::new();
    for round in 0..8 {
        let svc = Arc::clone(&svc);
        let uris = uris.clone();
        handles.push(std::thread::spawn(move || {
            for uri in &uris {
                let snapshot = svc.snapshot(uri).expect("snapshot");
                assert!(snapshot
                    .source_identity
                    .starts_with("mncs:source:artifact:"));
                if round % 2 == 0 {
                    let _ = svc.document_diagnostics(uri).expect("diagnostics");
                } else {
                    let _ = svc.workspace_symbols("fn").generation;
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker did not panic");
    }
}

#[test]
fn document_mutation_invalidates_exactly_once() {
    let svc = LanguageService::new(Some(fixtures_dir()));
    let uri = uri_for("bounded-iteration.mncs");
    let original = (*svc.store().content(&uri).expect("content")).clone();

    let baseline = Instant::now();
    svc.snapshot(&uri).expect("baseline");
    println!("cold analysis: {:?}", baseline.elapsed());

    let edited = original.replace("up_to 4", "up_to 3");
    svc.did_change(&uri, 2, edited).expect("change");

    let start = Instant::now();
    let changed = svc.snapshot(&uri).expect("changed snapshot");
    let reanalysis = start.elapsed();
    assert_ne!(
        changed.text(),
        original,
        "snapshot must reflect the new content"
    );

    let start = Instant::now();
    let again = svc.snapshot(&uri).expect("reuse after change");
    assert!(Arc::ptr_eq(&changed, &again));
    println!(
        "re-analysis after change: {reanalysis:?}; reuse lookup: {:?}",
        start.elapsed()
    );
}
