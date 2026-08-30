//! Service drift guard for the Forge MNCS-native source spine.
//!
//! The fixture imports the actual Forge core module and standard-library
//! identity/status modules from sibling checkouts. If module discovery or the
//! language dependency moves, this test fails at the service boundary.

use mncs_service_core::LanguageService;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn forge_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-forge-mcp")
}

fn language_library() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../mncs-language/library")
}

#[test]
fn native_forge_core_resolves_through_the_service_module_boundary() {
    let library = language_library();
    let forge = forge_root();
    if !library.join("core/identity.mncs").is_file()
        || !forge.join("mncs/forge/core.mncs").is_file()
    {
        return;
    }
    let library_path = std::env::join_paths([library, forge]).expect("library path encoding");
    std::env::set_var("MNCS_LIBRARY_PATH", library_path);

    let root = fixtures_dir();
    let service = LanguageService::new(Some(root.clone()));
    service.discover_workspace().expect("fixture discovery");
    let uri = format!(
        "file://{}",
        root.join("native-forge-service.mncs").display()
    );
    let snapshot = service
        .snapshot(&uri)
        .expect("native Forge fixture snapshot");

    assert!(
        snapshot.valid(),
        "native Forge fixture must elaborate: {:?}",
        snapshot
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code.clone(), diagnostic.message.clone()))
            .collect::<Vec<_>>()
    );
    let program = snapshot.front_end.program.as_ref().expect("linked program");
    assert!(program
        .functions
        .iter()
        .any(|function| function.name == "status_probe"));
}
