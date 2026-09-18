//! Differential tests for the second MNCS-native query kernel: bounded
//! symbol-kind filtering through `mncs.core.sequences.v1::count`.
//!
//! Each test executes the real `mncs-research-bytecode` backend and requires
//! `MNCS_LIBRARY_PATH` to resolve the standard library; when the sibling
//! checkout is absent the tests skip rather than fail.

use mncs_service_core::{LanguageService, ResponseStatus, SymbolKind};
use std::path::PathBuf;

fn language_library() -> PathBuf {
    let root = std::env::var_os("MNCS_LANGUAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-language")
        });
    root.join("library")
}

fn with_library() -> bool {
    let library = language_library();
    if !library.join("core/sequences.mncs").is_file() {
        return false;
    }
    std::env::set_var("MNCS_LIBRARY_PATH", &library);
    true
}

const PROBE: &str = "mncs 0.5;\n\nmodule probe.filter;\n\nenum Verdict { Pass, Fail }\n\nrecord Reading { celsius: i32 }\n\nfn adjust(delta: i64) -> (result: i64)\n{\n    let step: i64 = delta;\n    return step;\n}\n";

#[test]
fn native_kind_count_agrees_with_the_rust_control() {
    if !with_library() {
        return;
    }
    let svc = LanguageService::new(None);
    let uri = "untitled:native-filter-probe";
    svc.did_open(uri, 1, PROBE.to_owned()).expect("open");
    assert!(
        svc.snapshot(uri).expect("snapshot").valid(),
        "probe must elaborate"
    );

    // Index order for the probe: Module, FiniteType(Verdict), 2 variants,
    // RecordType(Reading), RecordField, Function(adjust), Parameter, Binding.
    // First 8 tags: [0, 5, 6, 6, 7, 8, 1, 2].
    for (kind, expected) in [
        (SymbolKind::Function, 1),
        (SymbolKind::FiniteVariant, 2),
        (SymbolKind::Binding, 0),
        (SymbolKind::Module, 1),
    ] {
        let response = svc.native_kind_count(uri, kind).expect("native filter");
        assert!(
            matches!(response.status, ResponseStatus::Answered),
            "{kind:?}: {:#?}",
            response
        );
        assert_eq!(response.reference_count, expected, "{kind:?}");
        assert_eq!(response.native_count, expected, "{kind:?}");
        let native = response.native.expect("native summary");
        assert_eq!(native.wanted_tag, mncs_service_core::symbol_kind_tag(kind));
    }
}

#[test]
fn native_kind_count_is_stable_across_repeated_calls() {
    if !with_library() {
        return;
    }
    let svc = LanguageService::new(None);
    let uri = "untitled:native-filter-stable";
    svc.did_open(uri, 1, PROBE.to_owned()).expect("open");
    let first = svc
        .native_kind_count(uri, SymbolKind::Function)
        .expect("first");
    let second = svc
        .native_kind_count(uri, SymbolKind::Function)
        .expect("second");
    assert!(matches!(first.status, ResponseStatus::Answered));
    assert_eq!(first.native_count, second.native_count);
    assert_eq!(
        first.native.map(|summary| summary.kernel_artifact_identity),
        second
            .native
            .map(|summary| summary.kernel_artifact_identity),
        "the frozen kernel artifact must be reused"
    );
}
