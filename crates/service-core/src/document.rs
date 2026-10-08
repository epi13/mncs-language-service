//! Workspace and document state.
//!
//! The store owns the service's operational notion of documents: which files
//! exist on disk, which are open in an editor, and what unsaved buffer text
//! overrides disk content. It does not interpret MNCS semantics; it only
//! provides exact, versioned text states that the analysis layer consumes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use mncs_syntax::{SourceArtifactKind, SourceEnvelope, SourceOrigin, SourceOriginKind};
use url::Url;

use crate::error::ServiceError;

/// Upper bound on documents discovered from disk in one workspace scan.
pub const MAX_DISCOVERED_DOCUMENTS: usize = 5_000;
/// Upper bound on a single document's size accepted for analysis.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Buffer {
    version: i32,
    content: StoredContent,
}

/// One exact text state with its authoritative derivations sealed once, at
/// store time. The identity is exactly `envelope(uri, text).identity`
/// (the envelope constructor is pure over `(uri, text)`), so warm queries
/// compare cached strings instead of re-hashing content per query.
#[derive(Debug, Clone)]
struct StoredContent {
    text: Arc<String>,
    /// Authoritative `mncs:source:artifact:...` identity for this exact
    /// `(uri, text)` pair.
    identity: String,
    /// Declared `module` name for this exact text, if any.
    module: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Document {
    path: Option<PathBuf>,
    /// Text as last read from disk. `None` for untitled buffers never saved.
    disk: Option<StoredContent>,
    /// Editor state overriding disk while the document is open.
    buffer: Option<Buffer>,
}

impl Document {
    fn content(&self) -> Option<Arc<String>> {
        self.stored().map(|stored| Arc::clone(&stored.text))
    }

    fn stored(&self) -> Option<&StoredContent> {
        if let Some(buffer) = &self.buffer {
            Some(&buffer.content)
        } else {
            self.disk.as_ref()
        }
    }

    /// Effective declared module: the winning content's sealed module.
    fn effective_module(&self) -> Option<&str> {
        self.stored().and_then(|stored| stored.module.as_deref())
    }

    fn open(&self) -> bool {
        self.buffer.is_some()
    }

    fn buffer_version(&self) -> Option<i32> {
        self.buffer.as_ref().map(|buffer| buffer.version)
    }
}

/// Monotonic workspace generation counter shared by all documents.
#[derive(Debug, Default)]
pub struct Generations(AtomicU64);

impl Generations {
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Restore a checkpoint floor without moving a live generation backward.
    pub fn restore_at_least(&self, value: u64) {
        let mut current = self.current();
        while current < value {
            match self
                .0
                .compare_exchange(current, value, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }
}

/// Resident workspace/document state for one service instance.
pub struct DocumentStore {
    documents: RwLock<BTreeMap<String, Document>>,
    root: RwLock<Option<PathBuf>>,
    /// Exact provider-selected source roots inside the workspace. `None`
    /// preserves the standalone whole-root behavior used by LSP clients that
    /// do not publish an Environment composition.
    discovery_roots: RwLock<Option<Vec<PathBuf>>>,
    generations: Generations,
    /// Serializes disk scans.
    discovery_lock: Mutex<()>,
    /// Resident module directory: declared module name -> claiming URIs.
    /// Maintained incrementally at every content mutation from the sealed
    /// per-doc module names, so import resolution is a map lookup instead
    /// of a workspace-wide header scan. The least URI wins, exactly as a
    /// fresh scan over the sorted document map would produce.
    module_claims: RwLock<BTreeMap<String, BTreeSet<String>>>,
    /// Monotonic store mutation counter: bumped on every content change,
    /// document registration/removal, and lazy disk load. Lets index
    /// validation skip per-document checks when nothing could have changed.
    content_version: AtomicU64,
    /// Content-change observer, wired once by the owning service. Invoked
    /// after every committed content change with no store guards held, so
    /// the service can invalidate derived state (transitive importers)
    /// regardless of which mutation path committed the change.
    change_callback: std::sync::OnceLock<ChangeCallback>,
}

/// Invoked with the changed URI after a content change commits.
pub(crate) type ChangeCallback = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

impl std::fmt::Debug for DocumentStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentStore")
            .field("documents", &self.documents)
            .field("root", &self.root)
            .field("generations", &self.generations)
            .field("module_claims", &self.module_claims)
            .field("content_version", &self.content_version)
            .field("change_callback", &self.change_callback.get().is_some())
            .finish_non_exhaustive()
    }
}

impl DocumentStore {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root: RwLock::new(root),
            discovery_roots: RwLock::new(None),
            documents: RwLock::new(BTreeMap::new()),
            generations: Generations::default(),
            discovery_lock: Mutex::new(()),
            module_claims: RwLock::new(BTreeMap::new()),
            content_version: AtomicU64::new(0),
            change_callback: std::sync::OnceLock::new(),
        }
    }

    /// Wire the content-change observer. Called once by the owning service;
    /// later calls are ignored.
    pub(crate) fn set_change_callback(&self, callback: ChangeCallback) {
        let _ = self.change_callback.set(callback);
    }

    fn notify_content_changed(&self, uri: &str) {
        if let Some(callback) = self.change_callback.get() {
            callback(uri);
        }
    }

    /// Current store mutation counter value.
    pub fn content_version(&self) -> u64 {
        self.content_version.load(Ordering::Relaxed)
    }

    fn bump_content_version(&self) {
        self.content_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Seal one exact text state for `uri`: authoritative envelope identity
    /// plus declared module name, computed once here instead of per query.
    fn seal_content(uri: &str, text: String) -> StoredContent {
        let identity = Self::envelope_for(uri, &text).identity;
        let module = mncs_syntax::declared_module_name(&text);
        StoredContent {
            text: Arc::new(text),
            identity,
            module,
        }
    }

    /// Record a document's effective-module change. Callers must invoke
    /// this while holding the documents write guard, so the claim update
    /// commits atomically with the content change it describes. Lock order
    /// is documents -> claims; the claims lock never nests outward.
    fn note_module_change(&self, uri: &str, old_module: Option<&str>, new_module: Option<&str>) {
        if old_module == new_module {
            return;
        }
        let Ok(mut claims) = self.module_claims.write() else {
            return;
        };
        if let Some(old) = old_module {
            if let Some(bucket) = claims.get_mut(old) {
                bucket.remove(uri);
                if bucket.is_empty() {
                    claims.remove(old);
                }
            }
        }
        if let Some(new) = new_module {
            claims
                .entry(new.to_owned())
                .or_default()
                .insert(uri.to_owned());
        }
    }

    /// Owning URI for a declared module name, if any resident document
    /// currently declares it. Least URI wins.
    pub(crate) fn module_uri(&self, module: &str) -> Option<String> {
        if let Some(uri) = self
            .module_claims
            .read()
            .ok()?
            .get(module)
            .and_then(|claimants| claimants.first().cloned())
        {
            return Some(uri);
        }
        // Miss: a registered-but-unloaded document may declare the module
        // (discovery registers without loading). Scan once, loading content
        // exactly as resolution always has, then retry.
        self.scan_modules_for(module)
    }

    /// Load every content-free document and claim its module, then retry one
    /// module lookup. The per-document load and claim commit atomically under
    /// the documents write guard.
    fn scan_modules_for(&self, module: &str) -> Option<String> {
        let uris: Vec<String> = self.read_documents().ok()?.keys().cloned().collect();
        for uri in uris {
            let Ok(mut documents) = self.write_documents() else {
                continue;
            };
            let Some(document) = documents.get_mut(&uri) else {
                continue;
            };
            if document.stored().is_some() {
                continue;
            }
            let path = document.path.clone().or_else(|| path_from_uri(&uri));
            let text = path.and_then(|path| {
                let text = fs::read_to_string(&path).ok()?;
                if text.len() > MAX_DOCUMENT_BYTES {
                    return None;
                }
                document.path = Some(path);
                Some(text)
            });
            let Some(text) = text else {
                continue;
            };
            let sealed = Self::seal_content(&uri, text);
            let new_module = sealed.module.clone();
            document.disk = Some(sealed);
            self.bump_content_version();
            self.note_module_change(&uri, None, new_module.as_deref());
        }
        self.module_claims
            .read()
            .ok()?
            .get(module)
            .and_then(|claimants| claimants.first().cloned())
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.root.read().ok().and_then(|root| root.clone())
    }

    /// Replace the workspace root. Existing documents remain known.
    pub fn set_root(&self, root: Option<PathBuf>) {
        if let Ok(mut current) = self.root.write() {
            *current = root;
        }
        if let Ok(mut roots) = self.discovery_roots.write() {
            *roots = None;
        }
    }

    /// Bind source discovery to exact roots selected by the workspace owner.
    /// The Language Service validates and canonicalizes these against its
    /// configured workspace before setting them.
    pub fn set_discovery_roots(&self, roots: Option<Vec<PathBuf>>) {
        if let Ok(mut current) = self.discovery_roots.write() {
            *current = roots;
        }
    }

    /// Roots currently used for discovery. Standalone mode defaults to the
    /// whole workspace root; composed mode reports the selected roots.
    pub fn discovery_roots(&self) -> Vec<PathBuf> {
        if let Ok(roots) = self.discovery_roots.read() {
            if let Some(roots) = roots.as_ref() {
                return roots.clone();
            }
        }
        self.workspace_root().into_iter().collect()
    }

    pub fn generation(&self) -> u64 {
        self.generations.current()
    }

    pub fn restore_generation_at_least(&self, value: u64) {
        self.generations.restore_at_least(value);
    }

    /// Reconcile the disk baseline against the last compact Language Service
    /// checkpoint.  The event log remains in-memory and bounded: only files
    /// whose current identity differs from the checkpoint consume a new
    /// generation.  With no checkpoint this establishes a quiet baseline.
    pub fn reconcile_checkpoint(
        &self,
        checkpoint: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<(String, u64)>, ServiceError> {
        let reconcile_started = std::time::Instant::now();
        let discovery_started = std::time::Instant::now();
        self.discover_workspace_impl(false)?;
        let discovery_us = crate::startup_profile::elapsed_us(discovery_started);
        let candidates: Vec<(String, PathBuf, bool)> = self
            .read_documents()?
            .iter()
            .filter_map(|(uri, document)| {
                let path = document.path.clone().or_else(|| path_from_uri(uri))?;
                Some((uri.clone(), path, document.open()))
            })
            .collect();
        let candidate_count = candidates.len();
        let mut read_count = 0usize;
        let mut scanned_bytes = 0u64;
        let mut read_us = 0u64;
        let mut seal_us = 0u64;
        let mut changed = Vec::new();
        for (uri, path, open) in candidates {
            if open {
                continue;
            }
            let read_started = std::time::Instant::now();
            let Ok(text) = fs::read_to_string(&path) else {
                read_us = read_us.saturating_add(crate::startup_profile::elapsed_us(read_started));
                continue;
            };
            read_us = read_us.saturating_add(crate::startup_profile::elapsed_us(read_started));
            if text.len() > MAX_DOCUMENT_BYTES {
                continue;
            }
            read_count = read_count.saturating_add(1);
            scanned_bytes = scanned_bytes.saturating_add(text.len() as u64);
            let seal_started = std::time::Instant::now();
            let sealed = Self::seal_content(&uri, text);
            seal_us = seal_us.saturating_add(crate::startup_profile::elapsed_us(seal_started));
            let differs = checkpoint
                .map(|values| values.get(&uri) != Some(&sealed.identity))
                .unwrap_or(false);
            let mut documents = self.write_documents()?;
            let Some(document) = documents.get_mut(&uri) else {
                continue;
            };
            if document.open() {
                continue;
            }
            let old_module = document.effective_module().map(str::to_owned);
            let old_identity = document.stored().map(|stored| stored.identity.clone());
            document.disk = Some(sealed);
            document.path = Some(path);
            self.bump_content_version();
            let new_module = document.effective_module().map(str::to_owned);
            let new_identity = document.stored().map(|stored| stored.identity.clone());
            self.note_module_change(&uri, old_module.as_deref(), new_module.as_deref());
            drop(documents);
            if old_identity != new_identity {
                self.notify_content_changed(&uri);
            }
            if differs {
                changed.push((uri, self.generations.next()));
            }
        }
        crate::startup_profile::emit(
            "checkpoint_reconcile",
            serde_json::json!({
                "discovery_us": discovery_us,
                "candidate_documents": candidate_count,
                "read_documents": read_count,
                "scanned_bytes": scanned_bytes,
                "read_us": read_us,
                "source_envelope_seal_us": seal_us,
                "changed_documents": changed.len(),
                "total_us": crate::startup_profile::elapsed_us(reconcile_started),
            }),
        );
        Ok(changed)
    }

    /// Discover `.mncs` files under the workspace root and register them as
    /// known on-disk documents (without loading contents).
    ///
    /// Already-known documents are left untouched: discovery never clobbers
    /// editor state or previously read disk text.
    fn discover_workspace_impl(&self, only_new: bool) -> Result<Vec<String>, ServiceError> {
        let _guard = self
            .discovery_lock
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace discovery is already running".to_owned(),
            })?;
        let root = self
            .workspace_root()
            .ok_or_else(|| ServiceError::WorkspaceUnavailable {
                path: "<no workspace root>".to_owned(),
            })?;
        if !root.is_dir() {
            return Err(ServiceError::WorkspaceUnavailable {
                path: root.display().to_string(),
            });
        }

        let mut discovered = Vec::new();
        let mut stack = self.discovery_roots();
        if stack.is_empty() {
            stack.push(root.to_path_buf());
        }
        while let Some(directory) = stack.pop() {
            if discovered.len() >= MAX_DISCOVERED_DOCUMENTS {
                break;
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // The Environment supplies exact selected repository roots.
                // Do not let a symlink inside one selected checkout expand
                // discovery into an unselected tree or create a directory
                // cycle.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    if name.starts_with('.') || name == "target" || name == "node_modules" {
                        continue;
                    }
                    stack.push(path);
                } else if file_type.is_file()
                    && name.ends_with(".mncs")
                    && discovered.len() < MAX_DISCOVERED_DOCUMENTS
                {
                    let uri = path_to_uri(&path);
                    let created = self
                        .ensure_document(&uri, Some(path))
                        .is_ok_and(|created| created);
                    if created || !only_new {
                        discovered.push(uri);
                    }
                }
            }
        }
        discovered.sort();
        Ok(discovered)
    }

    pub fn discover_workspace(&self) -> Result<Vec<String>, ServiceError> {
        self.discover_workspace_impl(false)
    }

    /// Discover files added after the resident baseline.  The service uses
    /// this narrow projection to turn a new filesystem document into one
    /// generation-bound change event without manufacturing startup events.
    pub fn discover_new_documents(&self) -> Result<Vec<String>, ServiceError> {
        self.discover_workspace_impl(true)
    }

    /// Load a newly discovered on-disk document and assign its first
    /// workspace generation. Existing buffers and disk baselines are left
    /// untouched.
    pub fn load_new_disk(&self, uri: &str) -> Result<Option<u64>, ServiceError> {
        let _guard = self
            .discovery_lock
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace refresh is already running".to_owned(),
            })?;
        let (path, open, has_disk) = self
            .read_documents()?
            .get(uri)
            .map(|document| {
                (
                    document.path.clone().or_else(|| path_from_uri(uri)),
                    document.open(),
                    document.disk.is_some(),
                )
            })
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        if open || has_disk {
            return Ok(None);
        }
        let Some(path) = path else {
            return Ok(None);
        };
        let text = fs::read_to_string(&path).map_err(|error| ServiceError::InvalidRequest {
            reason: format!(
                "could not read discovered document {}: {error}",
                path.display()
            ),
        })?;
        if text.len() > MAX_DOCUMENT_BYTES {
            return Ok(None);
        }
        let sealed = Self::seal_content(uri, text);
        let mut documents = self.write_documents()?;
        let Some(document) = documents.get_mut(uri) else {
            return Ok(None);
        };
        if document.open() || document.disk.is_some() {
            return Ok(None);
        }
        document.disk = Some(sealed);
        self.bump_content_version();
        let new_module = document.effective_module().map(str::to_owned);
        // Previously content-free: any declared module is a new claim.
        self.note_module_change(uri, None, new_module.as_deref());
        drop(documents);
        Ok(Some(self.generations.next()))
    }

    /// Reconcile known on-disk documents with their current filesystem bytes.
    /// Open buffers remain authoritative and are never clobbered.  The
    /// returned `(uri, generation)` pairs are consumed by the resident
    /// service so filesystem edits and LSP edits enter one event stream.
    pub fn refresh_disk(&self) -> Result<Vec<(String, u64)>, ServiceError> {
        Ok(self
            .refresh_disk_with(|_| true)?
            .into_iter()
            .filter_map(|(uri, generation, removed)| (!removed).then_some((uri, generation)))
            .collect())
    }

    /// Reconcile disk changes while allowing the owner to capture the
    /// previous semantic snapshot immediately before each changed document
    /// is committed. The callback runs only for a real update or removal.
    pub fn refresh_disk_with<F>(
        &self,
        mut before_change: F,
    ) -> Result<Vec<(String, u64, bool)>, ServiceError>
    where
        F: FnMut(&str) -> bool,
    {
        let _guard = self
            .discovery_lock
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace refresh is already running".to_owned(),
            })?;
        let candidates: Vec<(String, PathBuf, bool, Option<StoredContent>)> = self
            .read_documents()?
            .iter()
            .filter_map(|(uri, document)| {
                let path = document.path.clone().or_else(|| path_from_uri(uri))?;
                Some((uri.clone(), path, document.open(), document.disk.clone()))
            })
            .collect();
        let mut changed = Vec::new();
        for (uri, path, open, previous) in candidates {
            if open {
                continue;
            }
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if previous.is_none() {
                        continue;
                    }
                    if !before_change(&uri) {
                        continue;
                    }
                    let mut documents = self.write_documents()?;
                    let Some(document) = documents.get(&uri) else {
                        continue;
                    };
                    if document.open() || document.disk.is_none() {
                        continue;
                    }
                    let old_module = document.effective_module().map(str::to_owned);
                    documents.remove(&uri);
                    self.bump_content_version();
                    self.note_module_change(&uri, old_module.as_deref(), None);
                    drop(documents);
                    self.notify_content_changed(&uri);
                    changed.push((uri, self.generations.next(), true));
                    continue;
                }
                Err(_) => continue,
            };
            if text.len() > MAX_DOCUMENT_BYTES {
                continue;
            }
            // Discovery establishes the filesystem baseline.  A first refresh
            // must not manufacture a user edit event for every known file.
            if previous.is_none() {
                let sealed = Self::seal_content(&uri, text);
                let mut documents = self.write_documents()?;
                if let Some(document) = documents.get_mut(&uri) {
                    if !document.open() {
                        document.disk = Some(sealed);
                        self.bump_content_version();
                        let new_module = document.effective_module().map(str::to_owned);
                        self.note_module_change(&uri, None, new_module.as_deref());
                        drop(documents);
                    }
                }
                continue;
            }
            let same = previous.as_ref().map(|stored| stored.text.as_str()) == Some(text.as_str());
            if same {
                continue;
            }
            let _ = before_change(&uri);
            let sealed = Self::seal_content(&uri, text);
            let mut documents = self.write_documents()?;
            let Some(document) = documents.get_mut(&uri) else {
                continue;
            };
            if document.open() {
                continue;
            }
            let old_module = document.effective_module().map(str::to_owned);
            document.disk = Some(sealed);
            self.bump_content_version();
            let new_module = document.effective_module().map(str::to_owned);
            self.note_module_change(&uri, old_module.as_deref(), new_module.as_deref());
            drop(documents);
            // The caller verified the bytes differ, so this is a real change.
            self.notify_content_changed(&uri);
            let generation = self.generations.next();
            changed.push((uri, generation, false));
        }
        Ok(changed)
    }

    /// Ensure a document exists; returns whether it was newly created.
    fn ensure_document(&self, uri: &str, path: Option<PathBuf>) -> Result<bool, ServiceError> {
        let mut documents = self.write_documents()?;
        if documents.contains_key(uri) {
            return Ok(false);
        }
        documents.insert(
            uri.to_owned(),
            Document {
                path,
                disk: None,
                buffer: None,
            },
        );
        self.bump_content_version();
        Ok(true)
    }

    /// Register an opened editor document. Buffer text wins over disk until
    /// the document closes or saves.
    pub fn did_open(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(ServiceError::InvalidRequest {
                reason: format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"),
            });
        }
        let sealed = Self::seal_content(uri, text);
        let mut documents = self.write_documents()?;
        let generation = self.generations.next();
        let old_module = documents
            .get(uri)
            .and_then(|document| document.effective_module().map(str::to_owned));
        let path = documents
            .get(uri)
            .and_then(|document| document.path.clone())
            .or_else(|| path_from_uri(uri));
        let new_module = sealed.module.clone();
        documents.insert(
            uri.to_owned(),
            Document {
                path,
                disk: None,
                buffer: Some(Buffer {
                    version,
                    content: sealed,
                }),
            },
        );
        self.bump_content_version();
        self.note_module_change(uri, old_module.as_deref(), new_module.as_deref());
        drop(documents);
        self.notify_content_changed(uri);
        Ok(generation)
    }

    /// Apply a full-content change to an open document.
    pub fn did_change(&self, uri: &str, version: i32, text: String) -> Result<u64, ServiceError> {
        self.did_open(uri, version, text)
    }

    /// Apply one LSP `didChange` notification holding either a single
    /// full-document replacement or a sequence of ranged incremental edits.
    /// Ranged edits apply in order against the document's current content
    /// (unsaved buffer when open, otherwise disk text); the result becomes
    /// the new buffer, so incremental clients never resend whole files.
    pub fn did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<crate::edits::TextChange>,
    ) -> Result<u64, ServiceError> {
        if changes.iter().any(|change| change.range.is_none()) {
            let Some(last) = changes.into_iter().rfind(|change| change.range.is_none()) else {
                return Err(ServiceError::InvalidRequest {
                    reason: "empty change sequence".to_owned(),
                });
            };
            return self.did_open(uri, version, last.text);
        }
        let current = self
            .content(uri)
            .map(|text| (*text).clone())
            .unwrap_or_default();
        let next = crate::edits::apply_changes(&current, &changes);
        if next.len() > MAX_DOCUMENT_BYTES {
            return Err(ServiceError::InvalidRequest {
                reason: format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"),
            });
        }
        self.did_open(uri, version, next)
    }

    /// Record a save. The service never writes files itself: editors own
    /// persistence, and by the time an LSP `didSave` arrives the on-disk file
    /// already matches. The service only reconciles its resident copy so
    /// subsequent close/reopen cycles see consistent state.
    pub fn did_save(&self, uri: &str, text: Option<String>) -> Result<u64, ServiceError> {
        let mut documents = self.write_documents()?;
        let document = documents
            .get_mut(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        let old_identity = document.stored().map(|stored| stored.identity.clone());
        let old_module = document.effective_module().map(str::to_owned);
        let saved = text.or_else(|| {
            document
                .buffer
                .as_ref()
                .map(|buffer| (*buffer.content.text).clone())
        });
        if let Some(saved) = saved {
            let sealed = Self::seal_content(uri, saved);
            if document.disk.as_ref().map(|stored| &stored.identity) != Some(&sealed.identity) {
                document.disk = Some(sealed);
                self.bump_content_version();
            }
        }
        let new_module = document.effective_module().map(str::to_owned);
        let new_identity = document.stored().map(|stored| stored.identity.clone());
        let semantic_changed = old_identity != new_identity;
        self.note_module_change(uri, old_module.as_deref(), new_module.as_deref());
        drop(documents);
        if semantic_changed {
            self.generations.next();
            self.notify_content_changed(uri);
        }
        Ok(self.generations.current())
    }

    /// Close an editor document. Content reverts to the on-disk state; if the
    /// document was never backed by a file it is forgotten entirely.
    pub fn did_close(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        let mut documents = self.write_documents()?;
        self.generations.next();
        let document = documents
            .get_mut(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        let old_module = document.effective_module().map(str::to_owned);
        let had_unsaved = document.buffer.is_some() && document.disk.is_none();
        let had_buffer = document.buffer.is_some();
        document.buffer = None;
        if had_buffer {
            self.bump_content_version();
        }
        if had_unsaved && document.disk.is_none() && document.path.is_none() {
            documents.remove(uri);
            self.note_module_change(uri, old_module.as_deref(), None);
            drop(documents);
            self.notify_content_changed(uri);
            return Ok(None);
        }
        let new_module = document.effective_module().map(str::to_owned);
        let content = document.content().map(|text| (*text).clone());
        self.note_module_change(uri, old_module.as_deref(), new_module.as_deref());
        drop(documents);
        if had_buffer {
            self.notify_content_changed(uri);
        }
        Ok(content)
    }

    /// Load disk content lazily for a known document (e.g., discovered file
    /// queried before any editor open). Unknown URIs backed by an existing
    /// `.mncs` file are registered on first reference so clients may address
    /// workspace files without a prior scan.
    pub fn ensure_loaded(&self, uri: &str) -> Result<(), ServiceError> {
        {
            let documents = self.read_documents()?;
            if let Some(document) = documents.get(uri) {
                if document.content().is_some() {
                    return Ok(());
                }
            }
        }
        // Unknown or unloaded: register (if backed by a real .mncs file) and
        // load from disk.
        let known_path = self
            .read_documents()
            .ok()
            .and_then(|documents| {
                documents
                    .get(uri)
                    .and_then(|document| document.path.clone())
            })
            .or_else(|| path_from_uri(uri));
        let mut documents = self.write_documents()?;
        if !documents.contains_key(uri) {
            let Some(path) = known_path else {
                return Err(ServiceError::DocumentNotFound {
                    uri: uri.to_owned(),
                });
            };
            if !path.is_file() || !path.to_string_lossy().ends_with(".mncs") {
                return Err(ServiceError::DocumentNotFound {
                    uri: uri.to_owned(),
                });
            }
            documents.insert(
                uri.to_owned(),
                Document {
                    path: Some(path),
                    disk: None,
                    buffer: None,
                },
            );
            self.bump_content_version();
        }
        let document = documents
            .get_mut(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        if document.disk.is_none() && !document.open() {
            if let Some(path) = document.path.clone().or_else(|| path_from_uri(uri)) {
                if let Ok(text) = fs::read_to_string(&path) {
                    if text.len() <= MAX_DOCUMENT_BYTES {
                        document.disk = Some(Self::seal_content(uri, text));
                        self.bump_content_version();
                        document.path = Some(path);
                        let new_module = document.effective_module().map(str::to_owned);
                        // Previously content-free: any declared module is a new claim.
                        self.note_module_change(uri, None, new_module.as_deref());
                        drop(documents);
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Exact current content for the document: unsaved buffer when open,
    /// otherwise disk text.
    pub fn content(&self, uri: &str) -> Result<Arc<String>, ServiceError> {
        self.ensure_loaded(uri)?;
        let documents = self.read_documents()?;
        let document = documents
            .get(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        document
            .content()
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })
    }

    /// Whether the document currently has an open editor buffer.
    pub fn is_open(&self, uri: &str) -> Result<bool, ServiceError> {
        let documents = self.read_documents()?;
        documents
            .get(uri)
            .map(Document::open)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })
    }

    pub fn buffer_version(&self, uri: &str) -> Result<Option<i32>, ServiceError> {
        let documents = self.read_documents()?;
        documents
            .get(uri)
            .map(Document::buffer_version)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })
    }

    /// All known document URIs.
    pub fn workspace_root_path(&self) -> Option<String> {
        self.workspace_root().map(|path| path.display().to_string())
    }

    pub fn document_uris(&self) -> Vec<String> {
        self.read_documents()
            .map(|documents| documents.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether a URI is known at all.
    pub fn knows(&self, uri: &str) -> bool {
        self.read_documents()
            .map(|documents| documents.contains_key(uri))
            .unwrap_or(false)
    }

    /// Cached authoritative identity for the document's current content:
    /// exactly `envelope(uri, content).identity`, sealed when the content
    /// was stored. Warm queries must use this instead of re-sealing.
    pub fn content_identity(&self, uri: &str) -> Result<String, ServiceError> {
        self.ensure_loaded(uri)?;
        let documents = self.read_documents()?;
        let document = documents
            .get(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        document
            .stored()
            .map(|stored| stored.identity.clone())
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })
    }

    /// Cached declared `module` name for the document's current content.
    pub fn declared_module(&self, uri: &str) -> Result<Option<String>, ServiceError> {
        self.ensure_loaded(uri)?;
        let documents = self.read_documents()?;
        let document = documents
            .get(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        Ok(document.stored().and_then(|stored| stored.module.clone()))
    }

    /// Build the authoritative source envelope for the document's current
    /// content. The envelope identity doubles as the content fingerprint.
    /// Prefer [`Self::content_identity`] when only the fingerprint is needed.
    pub fn envelope(&self, uri: &str, text: &str) -> SourceEnvelope {
        Self::envelope_for(uri, text)
    }

    /// Pure envelope constructor shared by [`Self::envelope`] and the
    /// store-time sealing path, so cached identities are definitionally
    /// identical to freshly built envelopes.
    fn envelope_for(uri: &str, text: &str) -> SourceEnvelope {
        let origin_kind = if uri.starts_with("untitled:")
            || (!uri.starts_with("file:") && !Path::new(uri).is_absolute())
        {
            SourceOriginKind::Inline
        } else {
            SourceOriginKind::Uri
        };
        SourceEnvelope::new(
            SourceArtifactKind::Program,
            uri.to_owned(),
            SourceOrigin {
                kind: origin_kind,
                locator: Some(uri.to_owned()),
            },
            text.to_owned(),
        )
    }

    fn read_documents(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, BTreeMap<String, Document>>, ServiceError> {
        self.documents
            .read()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "document state poisoned".to_owned(),
            })
    }

    fn write_documents(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, BTreeMap<String, Document>>, ServiceError> {
        self.documents
            .write()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "document state poisoned".to_owned(),
            })
    }
}

pub(crate) fn path_to_uri(path: &Path) -> String {
    Url::from_file_path(path)
        .expect("filesystem paths must convert to file URIs")
        .to_string()
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::{DocumentStore, MAX_DOCUMENT_BYTES};
    use std::fs;
    use std::path::PathBuf;

    fn tempdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mncs-service-core-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    #[test]
    fn buffer_overrides_disk_until_save_and_close() {
        let dir = tempdir("buffer");
        let file = dir.join("a.mncs");
        fs::write(&file, "disk text\n").expect("write fixture");

        let store = DocumentStore::new(Some(dir));
        let uri = super::path_to_uri(&file);
        store
            .did_open(&uri, 1, "buffer text\n".to_owned())
            .expect("open");
        assert_eq!(
            (*store.content(&uri).expect("content")).clone(),
            "buffer text\n"
        );

        store
            .did_change(&uri, 2, "edited\n".to_owned())
            .expect("change");
        assert_eq!((*store.content(&uri).expect("content")).clone(), "edited\n");

        let changed_generation = store.generation();
        assert_eq!(
            store.did_save(&uri, None).expect("save"),
            changed_generation
        );
        assert_eq!(
            (*store.content(&uri).expect("content")).clone(),
            "edited\n",
            "save reconciles resident disk copy"
        );

        store.did_close(&uri).expect("close");
        assert_eq!((*store.content(&uri).expect("content")).clone(), "edited\n");
    }

    #[test]
    fn closing_without_disk_forgets_untitled_documents() {
        let store = DocumentStore::new(None);
        let uri = "untitled:scratch-1";
        store
            .did_open(uri, 1, "mncs 0.2;\n".to_owned())
            .expect("open");
        assert!(store.knows(uri));
        store.did_close(uri).expect("close");
        assert!(!store.knows(uri), "untitled docs vanish on close");
    }

    #[test]
    fn oversize_documents_are_rejected() {
        let store = DocumentStore::new(None);
        let big = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        let error = store
            .did_open("untitled:big", 1, big)
            .expect_err("oversize rejected");
        assert!(matches!(error, crate::ServiceError::InvalidRequest { .. }));
    }

    #[test]
    fn discovery_registers_mncs_files_only() {
        let dir = tempdir("discover");
        fs::write(dir.join("good.mncs"), "mncs 0.2;\n").expect("fixture");
        fs::write(dir.join("notes.txt"), "ignore me").expect("fixture");
        fs::create_dir_all(dir.join(".hidden")).expect("fixture");
        fs::write(dir.join(".hidden/secret.mncs"), "").expect("fixture");

        let store = DocumentStore::new(Some(dir.clone()));
        let found = store.discover_workspace().expect("discovery");
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("good.mncs"));
        assert!(store.knows(&found[0]));
        assert_eq!(
            (*store.content(&found[0]).expect("lazy load")).clone(),
            "mncs 0.2;\n"
        );
    }

    #[test]
    fn discovery_is_limited_to_selected_repository_roots() {
        let dir = tempdir("selected-discover");
        let selected = dir.join("selected");
        let unselected = dir.join("unselected");
        fs::create_dir_all(&selected).expect("selected root");
        fs::create_dir_all(&unselected).expect("unselected root");
        fs::write(selected.join("inside.mncs"), "mncs 0.2;\n").expect("selected source");
        fs::write(unselected.join("outside.mncs"), "mncs 0.2;\n").expect("unselected source");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&unselected, selected.join("linked-unselected"))
            .expect("selected-to-unselected symlink");

        let store = DocumentStore::new(Some(dir.clone()));
        store.set_discovery_roots(Some(vec![selected.clone()]));
        let found = store.discover_workspace().expect("selected discovery");
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("inside.mncs"));
        assert!(!found[0].contains("outside.mncs"));
        assert!(!found.iter().any(|uri| uri.contains("linked-unselected")));
    }

    #[test]
    fn discovery_after_baseline_returns_new_files_for_generation_binding() {
        let dir = tempdir("discover-new");
        fs::write(dir.join("initial.mncs"), "mncs 0.2;\n").expect("fixture");

        let store = DocumentStore::new(Some(dir.clone()));
        store.discover_workspace().expect("initial discovery");
        assert!(store.refresh_disk().expect("initial baseline").is_empty());

        fs::write(dir.join("new.mncs"), "mncs 0.2;\n").expect("new fixture");
        let new_documents = store.discover_new_documents().expect("new discovery");
        assert_eq!(new_documents.len(), 1);
        assert!(new_documents[0].ends_with("new.mncs"));
        let generation = store
            .load_new_disk(&new_documents[0])
            .expect("load new document")
            .expect("new document generation");
        assert_eq!(generation, 1);
        assert_eq!(
            store.content(&new_documents[0]).expect("content").as_str(),
            "mncs 0.2;\n"
        );
    }

    #[test]
    fn incremental_changes_apply_against_buffer_without_full_resend() {
        use crate::edits::{TextChange, TextRange};

        let store = DocumentStore::new(None);
        let uri = "untitled:incr-1";
        store
            .did_open(uri, 1, "let a: i64 = 1;\nlet b: i64 = 2;\n".to_owned())
            .expect("open");
        store
            .did_change_incremental(
                uri,
                2,
                vec![
                    TextChange {
                        range: Some(TextRange {
                            start_line: 0,
                            start_character: 4,
                            end_line: 0,
                            end_character: 5,
                        }),
                        text: "alpha".to_owned(),
                    },
                    TextChange {
                        range: Some(TextRange {
                            start_line: 1,
                            start_character: 13,
                            end_line: 1,
                            end_character: 14,
                        }),
                        text: "3".to_owned(),
                    },
                ],
            )
            .expect("incremental change");
        assert_eq!(
            (*store.content(uri).expect("content")).clone(),
            "let alpha: i64 = 1;\nlet b: i64 = 3;\n"
        );
        assert_eq!(
            store.buffer_version(uri).expect("version").expect("open"),
            2
        );
        // A full replacement inside a change sequence still works.
        store
            .did_change_incremental(
                uri,
                3,
                vec![TextChange {
                    range: None,
                    text: "fresh\n".to_owned(),
                }],
            )
            .expect("full change");
        assert_eq!((*store.content(uri).expect("content")).clone(), "fresh\n");
    }

    #[test]
    fn unknown_document_errors_are_explicit() {
        let store = DocumentStore::new(None);
        let error = store
            .content("file:///missing.mncs")
            .expect_err("not found");
        assert!(matches!(
            error,
            crate::ServiceError::DocumentNotFound { .. }
        ));
    }

    #[test]
    fn file_uris_round_trip_percent_encoded_paths() {
        let dir = tempdir("uri").join("space and unicode");
        fs::create_dir_all(&dir).expect("create nested directory");
        let file = dir.join("café.mncs");
        fs::write(&file, "mncs 0.2;\n").expect("write fixture");

        let uri = super::path_to_uri(&file);
        assert!(uri.contains("%20"), "URI should encode spaces: {uri}");
        let store = DocumentStore::new(Some(dir));
        assert_eq!(
            (*store.content(&uri).expect("encoded URI loads")).clone(),
            "mncs 0.2;\n"
        );
    }
}
