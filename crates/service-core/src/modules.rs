//! Module-import support: resolution of `use` declarations against resident
//! workspace documents and standard-library roots, plus dependency-aware
//! analysis invalidation.
//!
//! The authoritative semantics remain in mncs-language: this module only
//! decides *which source* satisfies an imported module name, then hands the
//! compiler core a resolver. A resolution miss is reported by the compiler
//! (`MNE173`) in the importing document.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mncs_compiler::ModuleResolver;
use mncs_syntax::{declared_module_name, module_names_compatible, SourceEnvelope};

use crate::document::DocumentStore;

/// Candidate file paths for one dotted module name under one root directory,
/// mirroring the research CLI's discovery layout: full dotted path, then a
/// version-tail-stripped path (`m.x.v1` -> `m/x.mncs`), then an
/// `mncs.`-prefix-stripped path (so `mncs.core.status.v1` can live at
/// `<root>/core/status.mncs`), then the bare final segment. Discovery only;
/// compatibility is established by elaborating the resolved source.
fn candidate_paths(root: &std::path::Path, module: &str) -> Vec<PathBuf> {
    let dotted = module.replace('.', "/");
    let mut paths = vec![root.join(format!("{dotted}.mncs"))];
    // `m.x.v1` -> `m/x.mncs`: drop a trailing `.vN` version segment.
    fn version_stripped(name: &str) -> Option<String> {
        let (head, last) = name.rsplit_once('.')?;
        let digits = last.strip_prefix('v')?;
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(head.to_owned())
    }
    if let Some(head) = version_stripped(module) {
        paths.push(root.join(format!("{}.mncs", head.replace('.', "/"))));
    }
    if let Some(rest) = module.strip_prefix("mncs.") {
        paths.push(root.join(format!("{}.mncs", rest.replace('.', "/"))));
        if let Some(rest_head) = version_stripped(rest) {
            paths.push(root.join(format!("{}.mncs", rest_head.replace('.', "/"))));
        }
    }
    paths.push(root.join(format!(
        "{}.mncs",
        module.rsplit('.').next().unwrap_or(module)
    )));
    paths
}

/// Directories that may satisfy `use` targets beyond resident documents,
/// read once per resolver construction from `MNCS_LIBRARY_PATH`
/// (`:`-separated), then standard-library roots from
/// [`discover_stdlib_root`]. This lets external consumers bind to
/// `mncs.core.*` without vendoring the standard-library tree into every
/// workspace and without manual configuration when `mncs-stdlib` sits
/// beside the workspace.
fn library_roots_from_env() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var("MNCS_LIBRARY_PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect();
    if let Some(stdlib) = discover_stdlib_root() {
        let library = stdlib.join("library");
        if !roots.iter().any(|root| root == &library) {
            roots.push(library);
        }
    }
    roots
}

/// Locates the `mncs-stdlib` checkout, mirroring the compiler CLI policy:
/// an explicit `MNCS_STDLIB_ROOT` wins (an empty value disables stdlib
/// discovery for hermetic sessions), otherwise `mncs-stdlib` beside the
/// current directory or beside its parent is used when it carries a
/// manifest. Returns the checkout root; the module tree lives under
/// `<root>/library`.
pub fn discover_stdlib_root() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("MNCS_STDLIB_ROOT") {
        if explicit.is_empty() {
            return None;
        }
        let root = PathBuf::from(explicit);
        return root.join("stdlib-manifest.json").is_file().then_some(root);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in [cwd.as_path(), cwd.parent()?].iter() {
        let candidate = base.join("mncs-stdlib");
        if candidate.join("stdlib-manifest.json").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `(module name, minimum profile)` pairs from the discovered stdlib
/// manifest, best-effort: an absent or unreadable manifest yields no
/// names rather than an error, so completion degrades to local symbols.
pub fn stdlib_manifest_modules() -> Vec<(String, String)> {
    let Some(root) = discover_stdlib_root() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(root.join("stdlib-manifest.json")) else {
        return Vec::new();
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    manifest
        .get("modules")
        .and_then(serde_json::Value::as_array)
        .map(|modules| {
            modules
                .iter()
                .filter_map(|entry| {
                    let name = entry.get("name")?.as_str()?.to_owned();
                    let min = entry
                        .get("requires_profile")
                        .and_then(|profile| profile.get("min"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?")
                        .to_owned();
                    Some((name, min))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves imported module names against documents known to the store:
/// open buffers first (authoritative editor state), then disk content of
/// registered documents, then configured standard-library roots. Names come
/// from each document's `module` declaration.
pub struct StoreResolver<'a> {
    store: &'a DocumentStore,
    /// module name -> uri, built once per analysis run.
    index: BTreeMap<String, String>,
    library_roots: Vec<PathBuf>,
}

impl<'a> StoreResolver<'a> {
    pub fn new(store: &'a DocumentStore) -> Self {
        let mut index = BTreeMap::new();
        for uri in store.document_uris() {
            if let Ok(text) = store.content(&uri) {
                if let Some(name) = declared_module_name(&text) {
                    index.entry(name).or_insert(uri);
                }
            }
        }
        Self {
            store,
            index,
            library_roots: library_roots_from_env(),
        }
    }
}

impl ModuleResolver for StoreResolver<'_> {
    fn resolve(&self, module: &str) -> Option<SourceEnvelope> {
        if !module.is_empty()
            && module
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
            && !module.starts_with('.')
            && !module.ends_with('.')
            && !module.contains("..")
        {
            if let Some(uri) = self.index.get(module) {
                if let Ok(text) = self.store.content(uri) {
                    let text = (*text).clone();
                    return Some(self.store.envelope(uri, &text));
                }
            }
            for root in &self.library_roots {
                for path in candidate_paths(root, module) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let Some(declared) = declared_module_name(&text) else {
                            continue;
                        };
                        if !module_names_compatible(module, &declared) {
                            continue;
                        }
                        let uri = format!("file://{}", path.display());
                        return Some(self.store.envelope(&uri, &text));
                    }
                }
            }
        }
        None
    }
}

/// Fingerprints of every direct dependency at analysis time, used to keep
/// cached analyses honest across multi-document edits: when any dependency's
/// content changes, dependent snapshots become stale even though their own
/// text is unchanged.
#[derive(Debug, Clone, Default)]
pub struct DependencyFingerprints {
    /// module name -> envelope identity observed while analyzing.
    pub modules: BTreeMap<String, String>,
}

impl DependencyFingerprints {
    /// Records the identity of every direct import of `text`, best-effort:
    /// unresolvable imports are absent here and surface as compiler
    /// diagnostics instead.
    pub fn collect(store: &DocumentStore, text: &str) -> Self {
        let resolver = StoreResolver::new(store);
        let mut modules = BTreeMap::new();
        for line in text.lines().map(str::trim_start) {
            let Some(rest) = line.strip_prefix("use ") else {
                continue;
            };
            let Some(name) = rest.trim_end().strip_suffix(';') else {
                continue;
            };
            let name = name.trim();
            if let Some(envelope) = resolver.resolve(name) {
                modules.insert(name.to_owned(), envelope.identity);
            }
        }
        Self { modules }
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}
