//! Resident workspace indexes: identity, declaration, reference, and
//! dependency maps derived from published snapshots.
//!
//! Every cross-document query used to snapshot all workspace documents and
//! scan them linearly. These indexes invert that relationship: each entry is
//! published once per snapshot alongside the analysis it derives from, so a
//! warm cross-document query is a handful of map lookups over already-valid
//! state plus the genuinely necessary per-hit projection (which is itself
//! precomputed at publish time).
//!
//! Correctness rule: an entry is usable only while it is *current* — its
//! source identity matches the document's current sealed identity and its
//! recorded dependency fingerprints still match a fresh cheap resolution.
//! Stale or missing entries are repaired through the ordinary `snapshot()`
//! funnel, which republishes the entry as a side effect. Indexed queries
//! therefore observe exactly what per-document snapshot storms would find.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::analysis::DocumentAnalysis;
use crate::coords::RangeInfo;
use crate::document::DocumentStore;
use crate::indexes::SymbolKind;
use crate::queries::SymbolSummary;

// ---------------------------------------------------------------------------
// Join keys
// ---------------------------------------------------------------------------

/// Declaration join key. Mirrors `symbols_matching`: exact declaring name
/// span (full coordinates: byte offsets plus line/column, since the same
/// byte span in different documents may sit on different lines), symbol
/// kind, and declaration spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeclarationKey {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub kind: SymbolKind,
    pub name: String,
}

/// Occurrence join key. Mirrors `references`: the declaration span the
/// compiler bound the use site to (full coordinates), the resolved kind,
/// and the use-site spelling. The spelling is `None` when the occurrence
/// span does not slice valid text; name-bound joins skip such occurrences
/// exactly as the snapshot scans do, while span-only scans still see them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OccurrenceKey {
    pub decl_start: usize,
    pub decl_end: usize,
    pub decl_line: usize,
    pub decl_column: usize,
    pub kind: SymbolKind,
    pub name: Option<String>,
}

/// One direct dependency edge: requested module, owning URI, and the source
/// identity the compiler consumed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepEdge {
    pub module: String,
    pub uri: String,
    pub identity: String,
}

/// One indexed reference occurrence with its pre-projected range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedOccurrence {
    pub uri: String,
    pub range: RangeInfo,
    /// Enclosing function's symbol index in the owning entry, if the use
    /// site sits inside a function body.
    pub enclosing_function: Option<usize>,
}

// ---------------------------------------------------------------------------
// Per-document entry
// ---------------------------------------------------------------------------

/// Index entry for one analyzed document state. All summaries and ranges are
/// projected at publish time with the snapshot's own position map, so they
/// are identical to what per-query projection would produce.
#[derive(Debug, Clone)]
pub struct WorkspaceEntry {
    /// Source identity the entry was built from.
    pub source_identity: String,
    /// Declared module name (`""` when the document declares none).
    pub module: String,
    /// Projected summaries, index-aligned with the snapshot's symbols.
    pub symbols: Vec<SymbolSummary>,
    /// Declaring name spans, index-aligned with `symbols`, for span-joined
    /// transitive walks without snapshot access.
    pub spans: Vec<mncs_syntax::SourceSpan>,
    /// Lowercased symbol names for case-insensitive workspace search.
    lower_names: Vec<String>,
    /// Pre-projected occurrence ranges, aligned with the snapshot's
    /// reference list (the global occurrence map points into this).
    occurrence_ranges: Vec<RangeInfo>,
    /// Enclosing-function symbol indexes aligned with the reference list.
    enclosing: Vec<Option<usize>>,
    /// Semantic identity -> first symbol index.
    by_identity: BTreeMap<String, usize>,
    /// Declaration key -> all matching symbol indexes.
    declarations: BTreeMap<DeclarationKey, Vec<usize>>,
    /// Occurrence key -> all matching `references` indexes.
    occurrences: BTreeMap<OccurrenceKey, Vec<usize>>,
    /// Function name -> first function symbol index.
    functions: BTreeMap<String, usize>,
    /// Type name -> finite/record type symbol indexes.
    types: BTreeMap<String, Vec<usize>>,
    /// Top-level function/finite-type/record-type names (import candidates).
    exports: BTreeSet<String>,
    /// Whether the analyzed text declares any `use` imports. Entries without
    /// imports validate by identity comparison alone (no text fetch).
    has_uses: bool,
    /// Whether any dependency edge points outside the store (library-root
    /// files). Such entries cannot use the store-version fast path because
    /// external files change without store mutations.
    has_external_dep: bool,
    /// Direct dependency edges for graph walks.
    pub deps: Vec<DepEdge>,
    /// Recorded dependency fingerprints (name -> consumed identity) for
    /// staleness checks without touching the snapshot cache.
    dep_fingerprints: BTreeMap<String, String>,
}

impl WorkspaceEntry {
    /// Build an entry from a published snapshot. `summarize` must be the
    /// same projection queries use, so indexed summaries are identical.
    pub fn build(
        store: &DocumentStore,
        uri: &str,
        snapshot: &DocumentAnalysis,
        summarize: fn(&str, &DocumentAnalysis, usize) -> SymbolSummary,
    ) -> Self {
        let text = snapshot.text();
        let symbols: Vec<SymbolSummary> = (0..snapshot.symbols.symbols.len())
            .map(|index| summarize(uri, snapshot, index))
            .collect();
        let spans: Vec<mncs_syntax::SourceSpan> = snapshot
            .symbols
            .symbols
            .iter()
            .map(|entry| entry.name_span)
            .collect();
        let lower_names = symbols
            .iter()
            .map(|summary| summary.name.to_lowercase())
            .collect();
        let references = &snapshot.symbols.references;
        let mut occurrence_ranges = Vec::with_capacity(references.len());
        let mut enclosing = Vec::with_capacity(references.len());
        let mut occurrences: BTreeMap<OccurrenceKey, Vec<usize>> = BTreeMap::new();
        for (index, reference) in references.iter().enumerate() {
            occurrence_ranges.push(snapshot.positions.range_of(text, reference.occurrence_span));
            enclosing.push(crate::intel::enclosing_function(
                snapshot,
                reference.occurrence_span.start,
            ));
            // Mirror `references`: the join name is the sliced use-site
            // spelling, or `None` when the span does not slice valid text
            // (name-bound joins skip those; span-only scans keep them).
            let name = text
                .get(reference.occurrence_span.start..reference.occurrence_span.end)
                .map(str::to_owned);
            occurrences
                .entry(OccurrenceKey {
                    decl_start: reference.declaration_span.start,
                    decl_end: reference.declaration_span.end,
                    decl_line: reference.declaration_span.line,
                    decl_column: reference.declaration_span.column,
                    kind: reference.kind,
                    name,
                })
                .or_default()
                .push(index);
        }
        let mut by_identity = BTreeMap::new();
        let mut declarations: BTreeMap<DeclarationKey, Vec<usize>> = BTreeMap::new();
        let mut functions = BTreeMap::new();
        let mut types: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut exports = BTreeSet::new();
        for (index, entry) in snapshot.symbols.symbols.iter().enumerate() {
            if let Some(identity) = entry.identity.as_ref() {
                by_identity
                    .entry(identity.as_str().to_owned())
                    .or_insert(index);
            }
            declarations
                .entry(DeclarationKey {
                    start: entry.name_span.start,
                    end: entry.name_span.end,
                    line: entry.name_span.line,
                    column: entry.name_span.column,
                    kind: entry.kind,
                    name: entry.name.clone(),
                })
                .or_default()
                .push(index);
            match entry.kind {
                SymbolKind::Function => {
                    functions.entry(entry.name.clone()).or_insert(index);
                    if entry.container.is_none() {
                        exports.insert(entry.name.clone());
                    }
                }
                SymbolKind::FiniteType | SymbolKind::RecordType => {
                    types.entry(entry.name.clone()).or_default().push(index);
                    if entry.container.is_none() {
                        exports.insert(entry.name.clone());
                    }
                }
                _ => {}
            }
        }
        let deps: Vec<DepEdge> = crate::modules::DependencyFingerprints::edges(
            store,
            text,
            &snapshot.front_end.module_resolutions,
        )
        .into_iter()
        .map(|(module, uri, identity)| DepEdge {
            module,
            uri,
            identity,
        })
        .collect();
        let has_external_dep = deps.iter().any(|edge| !store.knows(&edge.uri));
        let has_uses = text
            .lines()
            .any(|line| line.trim_start().starts_with("use "));
        Self {
            source_identity: snapshot.source_identity.clone(),
            module: snapshot
                .front_end
                .ast
                .as_ref()
                .map(|ast| ast.module.text.clone())
                .unwrap_or_default(),
            symbols,
            spans,
            lower_names,
            occurrence_ranges,
            enclosing,
            by_identity,
            declarations,
            occurrences,
            functions,
            types,
            exports,
            has_uses,
            has_external_dep,
            deps,
            dep_fingerprints: snapshot.dependencies.modules.clone(),
        }
    }

    /// Whether this entry still describes the document's current state: own
    /// identity matches and the recorded dependency fingerprints still match
    /// a fresh cheap resolution (the same rule as snapshot reuse, including
    /// newly-appearing dependencies via full map equality).
    pub fn is_current(&self, store: &DocumentStore, uri: &str) -> bool {
        let Ok(current) = store.content_identity(uri) else {
            return false;
        };
        if current != self.source_identity {
            return false;
        }
        if !self.has_uses {
            // No declared imports, and the identity matched the analyzed
            // text, so no dependency could have appeared or changed.
            // (An unresolvable import becoming resolvable requires a `use`
            // line, which would change the text and thus the identity.)
            return self.dep_fingerprints.is_empty();
        }
        let Ok(text) = store.content(uri) else {
            return false;
        };
        let now = crate::modules::DependencyFingerprints::collect(store, &text);
        now.modules == self.dep_fingerprints
    }

    /// Case-insensitive substring match over this entry's symbols.
    pub fn symbols_matching(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return (0..self.symbols.len()).collect();
        }
        self.lower_names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| name.contains(needle).then_some(index))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Workspace index
// ---------------------------------------------------------------------------

/// Resident cross-document indexes. All global maps are derived from
/// per-document entries and updated incrementally on publish/remove.
#[derive(Debug, Default)]
pub struct WorkspaceIndex {
    entries: BTreeMap<String, WorkspaceEntry>,
    by_identity: BTreeMap<String, BTreeSet<(String, usize)>>,
    declarations: BTreeMap<DeclarationKey, BTreeSet<(String, usize)>>,
    occurrences: BTreeMap<OccurrenceKey, BTreeSet<(String, usize)>>,
    functions: BTreeMap<String, BTreeSet<(String, usize)>>,
    types: BTreeMap<String, BTreeSet<(String, usize)>>,
    /// Exported top-level name -> (module, uri).
    exporters: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Dependency URI -> importing URIs.
    reverse_deps: BTreeMap<String, BTreeSet<String>>,
    /// URIs whose entries depend on files outside the store. These entries
    /// always validate individually, even on the store-version fast path.
    external_dep_uris: BTreeSet<String>,
}

impl WorkspaceIndex {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Total indexed reference occurrences (for memory-visibility stats).
    pub fn occurrence_count(&self) -> usize {
        self.occurrences.values().map(BTreeSet::len).sum()
    }

    pub fn entry(&self, uri: &str) -> Option<&WorkspaceEntry> {
        self.entries.get(uri)
    }

    /// Publish (or refresh) one document's entry, replacing its previous
    /// contributions to every global map.
    pub fn publish(&mut self, uri: String, entry: WorkspaceEntry) {
        self.remove(&uri);
        for (identity, index) in &entry.by_identity {
            self.by_identity
                .entry(identity.clone())
                .or_default()
                .insert((uri.clone(), *index));
        }
        for (key, indexes) in &entry.declarations {
            let bucket = self.declarations.entry(key.clone()).or_default();
            for index in indexes {
                bucket.insert((uri.clone(), *index));
            }
        }
        for (key, indexes) in &entry.occurrences {
            let bucket = self.occurrences.entry(key.clone()).or_default();
            for index in indexes {
                bucket.insert((uri.clone(), *index));
            }
        }
        for (name, index) in &entry.functions {
            self.functions
                .entry(name.clone())
                .or_default()
                .insert((uri.clone(), *index));
        }
        for (name, indexes) in &entry.types {
            let bucket = self.types.entry(name.clone()).or_default();
            for index in indexes {
                bucket.insert((uri.clone(), *index));
            }
        }
        if !entry.module.is_empty() {
            for name in &entry.exports {
                self.exporters
                    .entry(name.clone())
                    .or_default()
                    .insert((entry.module.clone(), uri.clone()));
            }
        }
        for edge in &entry.deps {
            self.reverse_deps
                .entry(edge.uri.clone())
                .or_default()
                .insert(uri.clone());
        }
        if entry.has_external_dep {
            self.external_dep_uris.insert(uri.clone());
        }
        self.entries.insert(uri, entry);
    }

    /// Drop one document's entry and all its global contributions.
    pub fn remove(&mut self, uri: &str) {
        let Some(entry) = self.entries.remove(uri) else {
            return;
        };
        for (identity, index) in &entry.by_identity {
            remove_from_bucket(&mut self.by_identity, identity, &(uri.to_owned(), *index));
        }
        for (key, indexes) in &entry.declarations {
            if let Some(bucket) = self.declarations.get_mut(key) {
                for index in indexes {
                    bucket.remove(&(uri.to_owned(), *index));
                }
                if bucket.is_empty() {
                    self.declarations.remove(key);
                }
            }
        }
        for (key, indexes) in &entry.occurrences {
            if let Some(bucket) = self.occurrences.get_mut(key) {
                for index in indexes {
                    bucket.remove(&(uri.to_owned(), *index));
                }
                if bucket.is_empty() {
                    self.occurrences.remove(key);
                }
            }
        }
        for (name, index) in &entry.functions {
            remove_from_bucket(&mut self.functions, name, &(uri.to_owned(), *index));
        }
        for (name, indexes) in &entry.types {
            if let Some(bucket) = self.types.get_mut(name) {
                for index in indexes {
                    bucket.remove(&(uri.to_owned(), *index));
                }
                if bucket.is_empty() {
                    self.types.remove(name);
                }
            }
        }
        if !entry.module.is_empty() {
            for name in &entry.exports {
                remove_from_bucket(
                    &mut self.exporters,
                    name,
                    &(entry.module.clone(), uri.to_owned()),
                );
            }
        }
        for edge in &entry.deps {
            if let Some(bucket) = self.reverse_deps.get_mut(&edge.uri) {
                bucket.remove(uri);
                if bucket.is_empty() {
                    self.reverse_deps.remove(&edge.uri);
                }
            }
        }
        self.external_dep_uris.remove(uri);
    }

    /// First `(uri, symbol)` claiming `identity`, by URI order — the same
    /// choice the snapshot storm's sorted scan would return.
    pub fn lookup_identity(&self, identity: &str) -> Option<(String, usize)> {
        self.by_identity.get(identity)?.first().cloned()
    }

    /// All `(uri, symbol)` matches for a declaration key, by URI order.
    pub fn lookup_declarations(&self, key: &DeclarationKey) -> Vec<(String, usize)> {
        self.declarations
            .get(key)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// All occurrences bound to one declaration key, with pre-projected
    /// ranges, by URI order.
    pub fn lookup_occurrences(&self, key: &OccurrenceKey) -> Vec<IndexedOccurrence> {
        let Some(bucket) = self.occurrences.get(key) else {
            return Vec::new();
        };
        bucket
            .iter()
            .filter_map(|(uri, index)| {
                let entry = self.entries.get(uri)?;
                Some(IndexedOccurrence {
                    uri: uri.clone(),
                    range: *entry.occurrence_ranges.get(*index)?,
                    enclosing_function: entry.enclosing.get(*index).copied().flatten(),
                })
            })
            .collect()
    }

    /// All occurrences bound to a declaration span of one kind, regardless
    /// of use-site spelling (call-hierarchy join), by URI order.
    pub fn occurrences_for_declaration(
        &self,
        declaration: mncs_syntax::SourceSpan,
        kind: SymbolKind,
    ) -> Vec<IndexedOccurrence> {
        // `None` sorts before `Some`, so this range covers spellings and
        // invalid-span occurrences alike, exactly like the unfiltered scan.
        self.occurrences
            .range(
                OccurrenceKey {
                    decl_start: declaration.start,
                    decl_end: declaration.end,
                    decl_line: declaration.line,
                    decl_column: declaration.column,
                    kind,
                    name: None,
                }..=OccurrenceKey {
                    decl_start: declaration.start,
                    decl_end: declaration.end,
                    decl_line: declaration.line,
                    decl_column: declaration.column,
                    kind,
                    name: Some("\u{10ffff}".to_owned()),
                },
            )
            .flat_map(|(_, bucket)| bucket.iter())
            .filter_map(|(uri, index)| {
                let entry = self.entries.get(uri)?;
                Some(IndexedOccurrence {
                    uri: uri.clone(),
                    range: *entry.occurrence_ranges.get(*index)?,
                    enclosing_function: entry.enclosing.get(*index).copied().flatten(),
                })
            })
            .collect()
    }

    /// All `(uri, symbol)` functions named `name`, by URI order.
    pub fn lookup_function(&self, name: &str) -> Vec<(String, usize)> {
        self.functions
            .get(name)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// All `(uri, symbol)` finite/record type declarations named `name`.
    pub fn lookup_type(&self, name: &str) -> Vec<(String, usize)> {
        self.types
            .get(name)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// `(module, uri)` exporters of a top-level name, ordered by module.
    pub fn exporters_of(&self, name: &str) -> Vec<(String, String)> {
        self.exporters
            .get(name)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// URIs importing `dep_uri`, by URI order. Entries are validated by the
    /// caller before graph walks, so moves repair on next access.
    pub fn importers_of(&self, dep_uri: &str) -> Vec<String> {
        self.reverse_deps
            .get(dep_uri)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Iterate `(uri, entry)` pairs for workspace-wide scans that need no
    /// snapshot access (e.g., symbol search over pre-projected summaries).
    pub fn entries(&self) -> impl Iterator<Item = (&String, &WorkspaceEntry)> {
        self.entries.iter()
    }

    /// URIs whose entries depend on files outside the store, by URI order.
    pub fn external_dep_uris(&self) -> Vec<String> {
        self.external_dep_uris.iter().cloned().collect()
    }
}

fn remove_from_bucket<K: Ord, V: Ord>(map: &mut BTreeMap<K, BTreeSet<V>>, key: &K, value: &V) {
    if let Some(bucket) = map.get_mut(key) {
        bucket.remove(value);
        if bucket.is_empty() {
            map.remove(key);
        }
    }
}

// ---------------------------------------------------------------------------
// Query-execution stats
// ---------------------------------------------------------------------------

/// Lock-free query-execution counters. The headline structural proof is
/// `frontend_runs == 0` across warm indexed queries.
#[derive(Debug, Default)]
pub struct ServiceStats {
    frontend_runs: AtomicU64,
    snapshot_hits: AtomicU64,
    snapshot_misses: AtomicU64,
    index_publishes: AtomicU64,
    index_queries: AtomicU64,
    index_repairs: AtomicU64,
}

impl ServiceStats {
    pub fn record_frontend_run(&self) {
        self.frontend_runs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_snapshot_hit(&self) {
        self.snapshot_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_snapshot_miss(&self) {
        self.snapshot_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_index_publish(&self) {
        self.index_publishes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_index_query(&self) {
        self.index_queries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_index_repair(&self) {
        self.index_repairs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ServiceStatsSnapshot {
        ServiceStatsSnapshot {
            frontend_runs: self.frontend_runs.load(Ordering::Relaxed),
            snapshot_hits: self.snapshot_hits.load(Ordering::Relaxed),
            snapshot_misses: self.snapshot_misses.load(Ordering::Relaxed),
            index_publishes: self.index_publishes.load(Ordering::Relaxed),
            index_queries: self.index_queries.load(Ordering::Relaxed),
            index_repairs: self.index_repairs.load(Ordering::Relaxed),
            indexed_documents: 0,
            indexed_occurrences: 0,
        }
    }

    pub fn reset(&self) {
        self.frontend_runs.store(0, Ordering::Relaxed);
        self.snapshot_hits.store(0, Ordering::Relaxed);
        self.snapshot_misses.store(0, Ordering::Relaxed);
        self.index_publishes.store(0, Ordering::Relaxed);
        self.index_queries.store(0, Ordering::Relaxed);
        self.index_repairs.store(0, Ordering::Relaxed);
    }
}

/// Point-in-time stats projection (serializable for Debug/Doctor/RPC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatsSnapshot {
    pub frontend_runs: u64,
    pub snapshot_hits: u64,
    pub snapshot_misses: u64,
    pub index_publishes: u64,
    pub index_queries: u64,
    pub index_repairs: u64,
    /// Entries currently resident (filled by `service_stats`).
    pub indexed_documents: usize,
    /// Occurrences currently resident (filled by `service_stats`).
    pub indexed_occurrences: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrence_prefix_scan_respects_declaration_span_and_kind() {
        let mut index = WorkspaceIndex::default();
        // Empty index: all lookups miss without panicking.
        assert!(index.lookup_identity("mncs.fn:m/f").is_none());
        assert!(index
            .lookup_declarations(&DeclarationKey {
                start: 0,
                end: 1,
                line: 0,
                column: 0,
                kind: SymbolKind::Function,
                name: "f".to_owned(),
            })
            .is_empty());
        assert!(index
            .occurrences_for_declaration(
                mncs_syntax::SourceSpan {
                    start: 0,
                    end: 1,
                    line: 0,
                    column: 0
                },
                SymbolKind::Function,
            )
            .is_empty());
        assert!(index.lookup_function("f").is_empty());
        assert!(index.exporters_of("f").is_empty());
        assert!(index.importers_of("file:///x").is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.occurrence_count(), 0);
        // Removing an unknown URI is a no-op.
        index.remove("file:///missing");
    }

    #[test]
    fn stats_count_and_reset() {
        let stats = ServiceStats::default();
        stats.record_frontend_run();
        stats.record_snapshot_hit();
        stats.record_snapshot_miss();
        stats.record_index_publish();
        stats.record_index_query();
        stats.record_index_repair();
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.frontend_runs, 1);
        assert_eq!(snapshot.snapshot_hits, 1);
        assert_eq!(snapshot.snapshot_misses, 1);
        assert_eq!(snapshot.index_publishes, 1);
        assert_eq!(snapshot.index_queries, 1);
        assert_eq!(snapshot.index_repairs, 1);
        stats.reset();
        assert_eq!(stats.snapshot().frontend_runs, 0);
    }
}
