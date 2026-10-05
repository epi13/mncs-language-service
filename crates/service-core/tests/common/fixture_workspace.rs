//! Per-test temporary copies of immutable source fixtures.
//!
//! The Language Service writes its restart checkpoint into the selected
//! workspace. Tests must never select the checked-in fixture tree as that
//! workspace, or parallel runs modify source state and leak host-specific
//! stream identities into Git.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct TemporaryRoot(PathBuf);

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

thread_local! {
    static ROOTS: RefCell<HashMap<PathBuf, TemporaryRoot>> = RefCell::new(HashMap::new());
}

/// Return a thread-local copy of a fixture tree with generated `.mncs`
/// checkpoints omitted. Rust's test harness gives each test its own thread,
/// so concurrent tests also receive isolated workspace roots.
pub fn root(relative_to_service_core: &str) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_to_service_core);
    let source = source.canonicalize().expect("fixture source directory");
    ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        if let Some(root) = roots.get(&source) {
            return root.0.clone();
        }
        let suffix = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let destination =
            std::env::temp_dir().join(format!("mncs-ls-fixture-{}-{suffix}", std::process::id()));
        fs::create_dir_all(&destination).expect("temporary fixture workspace");
        copy_tree(&source, &destination).expect("copy fixture workspace");
        roots.insert(source, TemporaryRoot(destination.clone()));
        destination
    })
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".mncs" {
            continue;
        }
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
