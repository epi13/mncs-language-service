//! Workspace and document state.
//!
//! The store owns the service's operational notion of documents: which files
//! exist on disk, which are open in an editor, and what unsaved buffer text
//! overrides disk content. It does not interpret MNCS semantics; it only
//! provides exact, versioned text states that the analysis layer consumes.

use std::collections::BTreeMap;
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
    text: Arc<String>,
}

#[derive(Debug, Clone, Default)]
struct Document {
    path: Option<PathBuf>,
    /// Text as last read from disk. `None` for untitled buffers never saved.
    disk: Option<Arc<String>>,
    /// Editor state overriding disk while the document is open.
    buffer: Option<Buffer>,
}

impl Document {
    fn content(&self) -> Option<Arc<String>> {
        if let Some(buffer) = &self.buffer {
            Some(Arc::clone(&buffer.text))
        } else {
            self.disk.clone()
        }
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
#[derive(Debug)]
pub struct DocumentStore {
    documents: RwLock<BTreeMap<String, Document>>,
    root: RwLock<Option<PathBuf>>,
    generations: Generations,
    /// Serializes disk scans.
    discovery_lock: Mutex<()>,
}

impl DocumentStore {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root: RwLock::new(root),
            documents: RwLock::new(BTreeMap::new()),
            generations: Generations::default(),
            discovery_lock: Mutex::new(()),
        }
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.root.read().ok().and_then(|root| root.clone())
    }

    /// Replace the workspace root. Existing documents remain known.
    pub fn set_root(&self, root: Option<PathBuf>) {
        if let Ok(mut current) = self.root.write() {
            *current = root;
        }
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
        self.discover_workspace_impl(false)?;
        let candidates: Vec<(String, PathBuf, bool)> = self
            .read_documents()?
            .iter()
            .filter_map(|(uri, document)| {
                let path = document.path.clone().or_else(|| path_from_uri(uri))?;
                Some((uri.clone(), path, document.open()))
            })
            .collect();
        let mut changed = Vec::new();
        for (uri, path, open) in candidates {
            if open {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.len() > MAX_DOCUMENT_BYTES {
                continue;
            }
            let identity = self.envelope(&uri, &text).identity;
            let differs = checkpoint
                .map(|values| values.get(&uri) != Some(&identity))
                .unwrap_or(false);
            let mut documents = self.write_documents()?;
            let Some(document) = documents.get_mut(&uri) else {
                continue;
            };
            if document.open() {
                continue;
            }
            document.disk = Some(Arc::new(text));
            document.path = Some(path);
            drop(documents);
            if differs {
                changed.push((uri, self.generations.next()));
            }
        }
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
        let mut stack = vec![root.to_path_buf()];
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
                if path.is_dir() {
                    if name.starts_with('.') || name == "target" || name == "node_modules" {
                        continue;
                    }
                    stack.push(path);
                } else if name.ends_with(".mncs") && discovered.len() < MAX_DISCOVERED_DOCUMENTS {
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
        let mut documents = self.write_documents()?;
        let Some(document) = documents.get_mut(uri) else {
            return Ok(None);
        };
        if document.open() || document.disk.is_some() {
            return Ok(None);
        }
        document.disk = Some(Arc::new(text));
        Ok(Some(self.generations.next()))
    }

    /// Reconcile known on-disk documents with their current filesystem bytes.
    /// Open buffers remain authoritative and are never clobbered.  The
    /// returned `(uri, generation)` pairs are consumed by the resident
    /// service so filesystem edits and LSP edits enter one event stream.
    pub fn refresh_disk(&self) -> Result<Vec<(String, u64)>, ServiceError> {
        let _guard = self
            .discovery_lock
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace refresh is already running".to_owned(),
            })?;
        let candidates: Vec<(String, PathBuf, bool, Option<Arc<String>>)> = self
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
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.len() > MAX_DOCUMENT_BYTES {
                continue;
            }
            // Discovery establishes the filesystem baseline.  A first refresh
            // must not manufacture a user edit event for every known file.
            if previous.is_none() {
                let mut documents = self.write_documents()?;
                if let Some(document) = documents.get_mut(&uri) {
                    if !document.open() {
                        document.disk = Some(Arc::new(text));
                    }
                }
                continue;
            }
            let same = previous.as_deref().map(String::as_str) == Some(text.as_str());
            if same {
                continue;
            }
            let mut documents = self.write_documents()?;
            let Some(document) = documents.get_mut(&uri) else {
                continue;
            };
            if document.open() {
                continue;
            }
            document.disk = Some(Arc::new(text));
            let generation = self.generations.next();
            changed.push((uri, generation));
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
        let mut documents = self.write_documents()?;
        let generation = self.generations.next();
        let path = documents
            .get(uri)
            .and_then(|document| document.path.clone())
            .or_else(|| path_from_uri(uri));
        documents.insert(
            uri.to_owned(),
            Document {
                path,
                disk: None,
                buffer: Some(Buffer {
                    version,
                    text: Arc::new(text),
                }),
            },
        );
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
        let _generation = self.generations.next();
        let document = documents
            .get_mut(uri)
            .ok_or_else(|| ServiceError::DocumentNotFound {
                uri: uri.to_owned(),
            })?;
        let saved = text.or_else(|| {
            document
                .buffer
                .as_ref()
                .map(|buffer| (*buffer.text).clone())
        });
        if let Some(saved) = saved {
            document.disk = Some(Arc::new(saved));
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
        let had_unsaved = document.buffer.is_some() && document.disk.is_none();
        document.buffer = None;
        if had_unsaved && document.disk.is_none() && document.path.is_none() {
            documents.remove(uri);
            return Ok(None);
        }
        Ok(document.content().map(|text| (*text).clone()))
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
                        document.disk = Some(Arc::new(text));
                        document.path = Some(path);
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

    /// Build the authoritative source envelope for the document's current
    /// content. The envelope identity doubles as the content fingerprint.
    pub fn envelope(&self, uri: &str, text: &str) -> SourceEnvelope {
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

        store.did_save(&uri, None).expect("save");
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
