//! Module-import support: resolution of `use` declarations against resident
//! workspace documents, and dependency-aware analysis invalidation.
//!
//! The authoritative semantics remain in mncs-language: this module only
//! decides *which source* satisfies an imported module name, then hands the
//! compiler core a resolver. A resolution miss is reported by the compiler
//! (`MNE173`) in the importing document.

use std::collections::BTreeMap;

use mncs_compiler::ModuleResolver;
use mncs_syntax::SourceEnvelope;

use crate::document::DocumentStore;

/// Extracts the declared `module <name>;` from source text without parsing.
/// Import resolution needs only the name, so a cheap scan keeps resolution
/// proportional to workspace size rather than AST size.
pub fn declared_module_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            let name = rest.trim_end();
            let name = name.strip_suffix(';').unwrap_or(name).trim();
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Resolves imported module names against documents known to the store:
/// open buffers first (authoritative editor state), then disk content of
/// registered documents. Names come from each document's `module` declaration.
pub struct StoreResolver<'a> {
    store: &'a DocumentStore,
    /// module name -> uri, built once per analysis run.
    index: BTreeMap<String, String>,
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
        Self { store, index }
    }
}

impl ModuleResolver for StoreResolver<'_> {
    fn resolve(&self, module: &str) -> Option<SourceEnvelope> {
        let uri = self.index.get(module)?;
        let text = self.store.content(uri).ok()?;
        let text = (*text).clone();
        Some(self.store.envelope(uri, &text))
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
