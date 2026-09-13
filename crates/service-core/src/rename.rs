//! Semantic rename: identity-bound workspace edits for one symbol.
//!
//! Rename resolves the symbol under the cursor through the compiler's
//! authoritative name resolutions, collects its declaration span plus every
//! bound reference occurrence (workspace-wide), validates the replacement
//! name against MNCS lexical rules, and rejects renames that would collide
//! with a visible declaration. Unrelated identifiers that merely share the
//! spelling are never touched: only spans bound to the resolved symbol by
//! `mncs-language` are edited.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::analysis::DocumentAnalysis;
use crate::coords::RangeInfo;
use crate::indexes::SymbolKind;
use crate::queries::{snapshot_info, ResponseStatus, ServiceError, SnapshotInfo};

/// One replacement inside a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SingleEdit {
    pub range: RangeInfo,
    pub new_text: String,
}

/// All replacements for one file, ordered by start byte, never overlapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEdit {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<SingleEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameResponse {
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FileEdit>,
    /// Non-fatal notes (e.g., documents skipped during collection).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

use crate::queries::LanguageService;

impl LanguageService {
    /// Compute a semantic rename of the symbol at `line:character` to
    /// `new_name`. Nothing is applied: the caller owns materialization.
    pub fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<RenameResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = || snapshot_info(uri, &snapshot);
        let fail = |reason: String| {
            Ok(RenameResponse {
                status: ResponseStatus::Unresolved { reason },
                snapshot: Some(info()),
                changes: Vec::new(),
                unresolved: Vec::new(),
            })
        };

        if let Some(reason) = invalid_rename_name(new_name) {
            return fail(reason);
        }
        let byte = snapshot
            .positions
            .offset_of(snapshot.text(), line, character);
        let Some((owner_uri, owner_snapshot, target)) = self.rename_target(uri, &snapshot, byte)
        else {
            return fail("no renamable symbol resolves at this position".to_owned());
        };
        let entry = &owner_snapshot.symbols.symbols[target];
        if entry.kind == SymbolKind::Module {
            return fail(
                "renaming a module renames its file and import graph; refusing".to_owned(),
            );
        }
        if entry.name == new_name {
            return fail("the new name is identical to the current name".to_owned());
        }
        if let Some(reason) = self.rename_collision(&owner_snapshot, target, new_name) {
            return fail(reason);
        }

        // Collect the declaration span plus every bound occurrence.
        let declaration = entry.name_span;
        let kind = entry.kind;
        let old_name = entry.name.clone();
        let mut per_file: std::collections::BTreeMap<String, Vec<(usize, usize)>> =
            std::collections::BTreeMap::new();
        let mut unresolved = Vec::new();

        // Declaration edit in the owning document.
        per_file.entry(owner_uri.clone()).or_default().push((
            owner_snapshot.symbols.symbols[target].name_span.start,
            owner_snapshot.symbols.symbols[target].name_span.end,
        ));

        let mut uris = self.store.document_uris();
        uris.sort();
        for candidate_uri in uris {
            let Ok(candidate) = self.snapshot(&candidate_uri) else {
                unresolved.push(format!("skipped unreachable document {candidate_uri}"));
                continue;
            };
            let text = candidate.text();
            for reference in &candidate.symbols.references {
                if reference.kind != kind || reference.declaration_span != declaration {
                    continue;
                }
                let Some(spelling) =
                    text.get(reference.occurrence_span.start..reference.occurrence_span.end)
                else {
                    continue;
                };
                if spelling != old_name {
                    continue;
                }
                per_file.entry(candidate_uri.clone()).or_default().push((
                    reference.occurrence_span.start,
                    reference.occurrence_span.end,
                ));
            }
        }

        let mut changes = Vec::new();
        for (file_uri, mut spans) in per_file {
            spans.sort();
            spans.dedup();
            let candidate = self.snapshot(&file_uri)?;
            let text = candidate.text();
            let mut edits = Vec::new();
            for (start, end) in spans {
                edits.push(SingleEdit {
                    range: candidate
                        .positions
                        .range_of(text, mncs_syntax::SourceSpan::at(text, start, end)),
                    new_text: new_name.to_owned(),
                });
            }
            changes.push(FileEdit {
                uri: file_uri,
                edits,
            });
        }
        changes.sort_by(|left, right| left.uri.cmp(&right.uri));

        Ok(RenameResponse {
            status: ResponseStatus::Answered,
            snapshot: Some(info()),
            changes,
            unresolved,
        })
    }

    /// Resolve the rename subject to its owning document, snapshot, and
    /// symbol index: local declarations directly, references through the
    /// workspace join shared with go-to-definition.
    fn rename_target(
        &self,
        uri: &str,
        snapshot: &DocumentAnalysis,
        byte: usize,
    ) -> Option<(String, Arc<DocumentAnalysis>, usize)> {
        if let Some(index) = snapshot.symbols.declaration_at(byte) {
            return Some((uri.to_owned(), self.snapshot(uri).ok()?, index));
        }
        let reference = snapshot.symbols.references_at(byte).next()?;
        if let Some(target) = reference.target {
            return Some((uri.to_owned(), self.snapshot(uri).ok()?, target));
        }
        // Imported symbol: join to the owning workspace document.
        let summary = self
            .targets_for_reference(uri, snapshot, reference)
            .into_iter()
            .next()?;
        let owner_uri = summary.uri?;
        let owner_snapshot = self.snapshot(&owner_uri).ok()?;
        let index = owner_snapshot.symbols.symbols.iter().position(|entry| {
            entry.name == summary.name
                && entry.name_span.start == summary.name_range.start_byte
                && entry.kind == summary.kind
        })?;
        Some((owner_uri, owner_snapshot, index))
    }

    /// Collision check against declarations visible from the target's scope.
    /// Conservative by construction: same-scope duplicates refuse, while
    /// shadowing-creating renames across scopes are allowed (the compiler
    /// remains the authority after the edit lands).
    fn rename_collision(
        &self,
        owner_snapshot: &DocumentAnalysis,
        target: usize,
        new_name: &str,
    ) -> Option<String> {
        let entry = &owner_snapshot.symbols.symbols[target];
        let same_scope = |candidate: &crate::indexes::SymbolEntry| {
            if candidate.name != new_name {
                return false;
            }
            match entry.kind {
                SymbolKind::Function | SymbolKind::FiniteType | SymbolKind::RecordType => {
                    // Module scope: any top-level declaration collides.
                    candidate.container.is_none()
                }
                SymbolKind::Parameter | SymbolKind::Binding | SymbolKind::IterationState => {
                    // Function scope: same-function locals and top-level
                    // names collide (a local would shadow the top level).
                    candidate.container == entry.container
                        || (candidate.container.is_none()
                            && !matches!(
                                candidate.kind,
                                SymbolKind::RecordField | SymbolKind::FiniteVariant
                            ))
                }
                SymbolKind::RecordField | SymbolKind::FiniteVariant => {
                    // Member scope: siblings under the same parent collide.
                    candidate.container == entry.container
                        && std::mem::discriminant(&candidate.kind)
                            == std::mem::discriminant(&entry.kind)
                }
                SymbolKind::Module => true,
            }
        };
        if owner_snapshot
            .symbols
            .symbols
            .iter()
            .enumerate()
            .any(|(index, candidate)| index != target && same_scope(candidate))
        {
            return Some(format!(
                "`{new_name}` already names a declaration visible from `{}`; refusing",
                entry.name
            ));
        }
        None
    }
}

/// Validate a proposed name against MNCS lexical rules without retyping
/// anything: non-empty, identifier start/continue classes matching the
/// language lexer (`_` or alphabetic start, `_` or alphanumeric continue),
/// and neither a reserved keyword nor a builtin scalar type name.
fn invalid_rename_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("the new name is empty".to_owned());
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    let start_ok = first == '_' || first.is_alphabetic();
    if !start_ok || !chars.all(|c| c == '_' || c.is_alphanumeric()) {
        return Some(format!("`{name}` is not a valid MNCS identifier"));
    }
    if crate::render::KEYWORDS.contains(&name) {
        return Some(format!("`{name}` is a reserved MNCS keyword"));
    }
    if crate::indexes::is_builtin_type(name) {
        return Some(format!("`{name}` is a builtin scalar type name"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::invalid_rename_name;

    #[test]
    fn rename_names_follow_lexical_rules() {
        assert!(invalid_rename_name("alpha_1").is_none());
        assert!(invalid_rename_name("_hidden").is_none());
        assert!(invalid_rename_name("éclair").is_none());
        assert!(invalid_rename_name("").is_some());
        assert!(invalid_rename_name("1abc").is_some());
        assert!(invalid_rename_name("has space").is_some());
        assert!(invalid_rename_name("fn").is_some());
        assert!(invalid_rename_name("return").is_some());
        assert!(invalid_rename_name("i64").is_some());
        assert!(invalid_rename_name("has-hyphen").is_some());
    }
}
