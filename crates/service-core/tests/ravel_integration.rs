//! RAVEL-driven integration: a real linked multi-module workspace that
//! imports the standard library through `MNCS_LIBRARY_PATH`, plus
//! candidate analysis against resident dependencies.

use mncs_service_core::LanguageService;
use std::path::PathBuf;

fn configure_ravel_workspace() -> Option<PathBuf> {
    // The RAVEL checkout is a sibling of this repository in development
    // environments; when absent, these tests are skipped rather than failed,
    // and CI exercises the equivalent fixtures instead. The standard-library
    // root is exported exactly as an external consumer would export it.
    let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../RAVEL/mncs/workspace");
    let library = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-language/library");
    if candidate.join("ravel/core.mncs").is_file() && library.join("core/status.mncs").is_file() {
        std::env::set_var("MNCS_LIBRARY_PATH", &library);
        Some(candidate)
    } else {
        None
    }
}

fn uri_for(workspace: &std::path::Path, relative: &str) -> String {
    format!("file://{}", workspace.join(relative).display())
}

/// Every RAVEL module must elaborate cleanly through the service when the
/// standard-library root is exported: imports of mncs.core.* resolve through
/// MNCS_LIBRARY_PATH, and imports of ravel.types.v1 resolve against
/// resident workspace documents.
#[test]
fn ravel_modules_resolve_through_library_path_and_resident_documents() {
    let Some(workspace) = configure_ravel_workspace() else {
        return;
    };
    let svc = LanguageService::new(Some(workspace.clone()));
    svc.discover_workspace().expect("discovery");

    for relative in [
        "ravel/types.mncs",
        "ravel/core.mncs",
        "ravel/loop.mncs",
        "ravel/checkpoint.mncs",
        "ravel/memory.mncs",
        "ravel/task.mncs",
        "ravel/lifecycle.mncs",
        "ravel/provider.mncs",
        "ravel/budget.mncs",
        "ravel/forge.mncs",
    ] {
        let snapshot = svc
            .snapshot(&uri_for(&workspace, relative))
            .expect("snapshot");
        assert!(
            snapshot.valid(),
            "{relative} must elaborate: {:?}",
            snapshot
                .diagnostics()
                .iter()
                .map(|d| (d.code.clone(), d.message.clone()))
                .collect::<Vec<_>>()
        );
    }
}

/// Editing an importing module's text must produce identity-bound candidate
/// deltas even though the module has `use` dependencies: the candidate
/// elaborates with resident resolution, so no false MNE173 appears.
#[test]
fn candidate_analysis_resolves_against_resident_dependencies() {
    let Some(workspace) = configure_ravel_workspace() else {
        return;
    };
    let svc = LanguageService::new(Some(workspace.clone()));
    svc.discover_workspace().expect("discovery");

    let task_uri = uri_for(&workspace, "ravel/task.mncs");
    let baseline = svc.snapshot(&task_uri).expect("baseline");
    assert!(baseline.valid());

    let edited = baseline.text().replace(
        "fn affordable(context: TaskContext, requested_steps: i64) -> (result: bool) {
    return requested_steps <= context.budget_steps;
}",
        "fn affordable(context: TaskContext, requested_steps: i64) -> (result: bool) {
    return requested_steps < context.budget_steps;
}",
    );
    assert_ne!(edited, baseline.text(), "edit must apply");

    let response = svc
        .analyze_candidate(&task_uri, &edited)
        .expect("candidate");
    assert_eq!(response.status, mncs_service_core::ResponseStatus::Answered);
    assert!(response.changed);
    assert!(response.candidate_elaborates, "candidate must elaborate");
    let semantic = response.semantic.as_ref().expect("semantic delta");
    assert!(
        !semantic.changed.is_empty(),
        "the edited function must appear as a changed identity"
    );
    // The baseline document is untouched by candidate analysis.
    let resident = svc.snapshot(&task_uri).expect("resident");
    assert_eq!(resident.source_identity, response.baseline_source_identity);
    assert_ne!(resident.source_identity, response.candidate_source_identity);
}
