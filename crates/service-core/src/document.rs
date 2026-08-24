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
        self.0.fetch_add(1, Ordering::Relaxed)
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Resident workspace/document state for one service instance.
#[derive(Debug)]
pub struct DocumentStore {
    root: Option<PathBuf>,
    documents: RwLock<BTreeMap<String, Document>>,
    generations: Generations,
    /// Serializes disk scans.
    discovery_lock: Mutex<()>,
}

impl DocumentStore {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            documents: RwLock::new(BTreeMap::new()),
            generations: Generations::default(),
            discovery_lock: Mutex::new(()),
        }
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn generation(&self) -> u64 {
        self.generations.current()
    }

    /// Discover `.mncs` files under the workspace root and register them as
    /// known on-disk documents (without loading contents).
    ///
    /// Already-known documents are left untouched: discovery never clobbers
    /// editor state or previously read disk text.
    pub fn discover_workspace(&self) -> Result<Vec<String>, ServiceError> {
        let _guard = self
            .discovery_lock
            .lock()
            .map_err(|_| ServiceError::InvalidRequest {
                reason: "workspace discovery is already running".to_owned(),
            })?;
        let root = self
            .root
            .as_ref()
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
                    if self.ensure_document(&uri, Some(path)).is_ok() {
                        discovered.push(uri);
                    }
                }
            }
        }
        discovered.sort();
        Ok(discovered)
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
        let origin_kind = if uri.starts_with("untitled:") || !Path::new(uri).is_absolute() {
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
    format!("file://{}", path.display())
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    if stripped.starts_with('/') {
        Some(PathBuf::from(stripped))
    } else {
        None
    }
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
}
