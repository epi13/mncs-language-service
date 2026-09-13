//! Code actions: safe, semantically defensible editing assists.
//!
//! Only transformations with a clear authoritative basis are offered. Today
//! that is exactly one family: when elaboration reports an unresolvable call
//! target (`MNE131`) and the workspace (or standard-library roots) exports a
//! same-named module-level subject from another module, offer to add the
//! missing `use` import. The edit inserts one import line after the module
//! declaration; nothing else in the file is touched.

use serde::{Deserialize, Serialize};

use crate::coords::RangeInfo;
use crate::indexes::SymbolKind;
use crate::queries::{snapshot_info, ResponseStatus, ServiceError, SnapshotInfo};
use crate::rename::FileEdit;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    /// LSP-style kind (`quickfix`).
    pub kind: String,
    /// Codes of the diagnostics this action addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<FileEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeActionsResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<CodeAction>,
}

use crate::queries::LanguageService;

/// Elaboration code for an unresolvable call target.
const UNRESOLVED_CALL_CODE: &str = "MNE131";
const MAX_IMPORT_ACTIONS: usize = 3;

impl LanguageService {
    /// Code actions for the range `start..end` (LSP line/UTF-16 coordinates).
    /// Only diagnostics overlapping the range seed actions.
    pub fn code_actions(
        &self,
        uri: &str,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Result<CodeActionsResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let text = snapshot.text();
        let start = snapshot
            .positions
            .offset_of(text, start_line, start_character);
        let end = snapshot.positions.offset_of(text, end_line, end_character);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };

        let mut actions = Vec::new();
        for diagnostic in snapshot.diagnostics() {
            if diagnostic.code != UNRESOLVED_CALL_CODE {
                continue;
            }
            if diagnostic.span.end < start || diagnostic.span.start > end {
                continue;
            }
            let Some(name) = text
                .get(diagnostic.span.start..diagnostic.span.end)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            for candidate in self.exporters_of(name, uri) {
                if actions.len() >= MAX_IMPORT_ACTIONS {
                    break;
                }
                let Some(edit) = self.missing_import_edit(uri, &snapshot, &candidate) else {
                    continue;
                };
                actions.push(CodeAction {
                    title: format!("Add `use {};` for `{name}`", candidate.module),
                    kind: "quickfix".to_owned(),
                    diagnostics: vec![diagnostic.code.clone()],
                    edit: Some(edit),
                });
            }
        }
        actions.sort_by(|left, right| left.title.cmp(&right.title));
        actions.dedup_by(|left, right| left.title == right.title);
        Ok(CodeActionsResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(snapshot_info(uri, &snapshot)),
            actions,
        })
    }

    /// Module-level subjects named `name` exported from other modules:
    /// `(module name, defining URI)`, ordered deterministically.
    fn exporters_of(&self, name: &str, except_uri: &str) -> Vec<ExportCandidate> {
        let mut candidates = Vec::new();
        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            if candidate_uri == except_uri {
                continue;
            }
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                continue;
            };
            let module = candidate
                .front_end
                .ast
                .as_ref()
                .map(|ast| ast.module.text.clone())
                .unwrap_or_default();
            if module.is_empty() {
                continue;
            }
            let exports = candidate.symbols.symbols.iter().any(|entry| {
                entry.name == name
                    && entry.container.is_none()
                    && matches!(
                        entry.kind,
                        SymbolKind::Function | SymbolKind::FiniteType | SymbolKind::RecordType
                    )
            });
            if exports {
                candidates.push(ExportCandidate {
                    module,
                    uri: candidate_uri,
                });
            }
        }
        candidates.sort_by(|left, right| {
            left.module
                .cmp(&right.module)
                .then(left.uri.cmp(&right.uri))
        });
        candidates.dedup_by(|left, right| left.module == right.module);
        candidates.truncate(MAX_IMPORT_ACTIONS);
        candidates
    }

    /// A one-line `use` insertion after the module declaration, or `None`
    /// when the module line is missing or the import already exists.
    fn missing_import_edit(
        &self,
        uri: &str,
        snapshot: &crate::analysis::DocumentAnalysis,
        candidate: &ExportCandidate,
    ) -> Option<FileEdit> {
        let text = snapshot.text();
        // Never offer an import that is already present.
        if snapshot.front_end.ast.as_ref().is_some_and(|ast| {
            ast.uses
                .iter()
                .any(|used| used.module.text == candidate.module)
        }) {
            return None;
        }
        let ast = snapshot.front_end.ast.as_ref()?;
        let anchor = snapshot.positions.position_of(text, ast.module.span.end);
        // Insert at the start of the line following the module declaration.
        let insert_line = anchor.line + 1;
        let insert_byte = snapshot.positions.offset_of(text, insert_line, 0);
        let new_text = format!("use {};\n", candidate.module);
        Some(FileEdit {
            uri: uri.to_owned(),
            edits: vec![crate::rename::SingleEdit {
                range: RangeInfo {
                    start_line: insert_line,
                    start_character: 0,
                    end_line: insert_line,
                    end_character: 0,
                    start_byte: insert_byte,
                    end_byte: insert_byte,
                },
                new_text,
            }],
        })
    }
}

#[derive(Debug, Clone)]
struct ExportCandidate {
    module: String,
    uri: String,
}
