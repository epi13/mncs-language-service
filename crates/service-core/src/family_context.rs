//! Bounded family preflight for agents.
//!
//! This module composes read-only projections from the authorities that own
//! them.  The language capability index remains owned by `mncs-language`;
//! Commons owns family architecture, pressure, and shadow facts; Atlas is an
//! optional non-normative orientation projection.  The service owns only the
//! bounded response shape and query policy.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::language_knowledge::{self, LanguageDelta, LanguageModule, LanguageTopic};
use crate::{ResponseStatus, ServiceError};

pub const FAMILY_CONTEXT_SCHEMA: &str = "mncs.family-agent-context/1";
const ARCHITECTURE_SCHEMA: &str = "commons.mncs.architecture-model/1";
const MAX_CONTEXT_ITEMS: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyAgentContextResponse {
    pub schema_version: String,
    pub status: ResponseStatus,
    pub repository: Option<RepositoryContext>,
    pub language: LanguageContext,
    pub architecture: ArchitectureContext,
    /// Atlas is deliberately labelled as orientation: it never replaces the
    /// local manifest or Commons facts in this response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas: Option<AtlasContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pressures: Vec<PressureSummary>,
    pub completeness: ContextCompleteness,
    pub provenance: Vec<ContextSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryContext {
    pub repository: Option<String>,
    pub revision: Option<u64>,
    pub manifest_path: String,
    pub manifest_identity: String,
    pub authority: Option<Value>,
    pub contracts: Option<Value>,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageContext {
    pub source_path: Option<String>,
    pub current_profile: Option<String>,
    pub content_identity: Option<String>,
    pub compiler_inventory_identity: Option<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<LanguageTopic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<LanguageModule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<LanguageDelta>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureContext {
    pub source_path: Option<String>,
    pub schema_identity: Option<String>,
    pub content_identity: Option<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generators: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<Value>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasContext {
    pub source_path: String,
    pub registry_identity: Option<String>,
    pub registry_revision: Option<String>,
    pub authority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PressureSummary {
    pub id: String,
    pub title: Option<String>,
    pub target: Option<String>,
    pub domain: Option<String>,
    pub severity: Option<String>,
    pub status: Option<String>,
    pub verification_state: Option<String>,
    pub unresolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_repositories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_behavior: Option<String>,
    pub source_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompleteness {
    pub complete: bool,
    pub state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSource {
    pub authority: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

pub fn query(
    root: Option<&Path>,
    repository: Option<&str>,
    topic: Option<&str>,
    symbol: Option<&str>,
    known_language_identity: Option<&str>,
    known_architecture_identity: Option<&str>,
    max_items: usize,
) -> Result<FamilyAgentContextResponse, ServiceError> {
    if max_items == 0 {
        return Err(ServiceError::InvalidRequest {
            reason: "max_items must be greater than zero".to_owned(),
        });
    }
    let max_items = max_items.min(MAX_CONTEXT_ITEMS);
    let workspace = root
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        .or_else(|| env::current_dir().ok());

    let mut limitations = Vec::new();
    let (local_repository, mut repository_limitations, manifest_source) =
        load_repository_manifest(workspace.as_deref());
    limitations.append(&mut repository_limitations);
    let repository_id = local_repository
        .as_ref()
        .and_then(|item| item.repository.as_deref())
        .or(repository)
        .map(str::to_owned);
    if let (Some(requested), Some(actual)) = (
        repository,
        local_repository
            .as_ref()
            .and_then(|item| item.repository.as_deref()),
    ) {
        if requested != actual {
            limitations.push(format!(
                "requested repository {requested} does not match local manifest {actual}"
            ));
        }
    }

    let mut provenance = Vec::new();
    if let Some(source) = manifest_source {
        provenance.push(source);
    }

    let language = match language_knowledge::query(
        workspace.as_deref(),
        topic,
        symbol,
        None,
        None,
        known_language_identity,
        max_items,
    ) {
        Ok(response) => {
            provenance.push(ContextSource {
                authority: "mncs-language".to_owned(),
                path: response.source_path.clone(),
                identity: Some(response.content_identity.clone()),
            });
            LanguageContext {
                source_path: Some(response.source_path),
                current_profile: Some(response.current_profile),
                content_identity: Some(response.content_identity),
                compiler_inventory_identity: response.compiler_inventory_identity,
                mode: response.projection.mode,
                topics: response.topics,
                modules: response.modules,
                delta: response.delta,
                complete: response.projection.complete,
                limitations: response.projection.limitations,
            }
        }
        Err(error) => {
            limitations.push(format!(
                "language capability projection unavailable: {error}"
            ));
            LanguageContext {
                source_path: None,
                current_profile: None,
                content_identity: None,
                compiler_inventory_identity: None,
                mode: "unknown".to_owned(),
                topics: Vec::new(),
                modules: Vec::new(),
                delta: None,
                complete: false,
                limitations: vec![error.to_string()],
            }
        }
    };

    let commons_root = discover_commons_root(workspace.as_deref());
    let (architecture, mut architecture_limitations, architecture_sources) = load_architecture(
        commons_root.as_deref(),
        repository_id.as_deref(),
        known_architecture_identity,
        max_items,
    );
    limitations.append(&mut architecture_limitations);
    provenance.extend(architecture_sources);

    let (pressures, pressure_source) =
        load_pressures(commons_root.as_deref(), repository_id.as_deref(), max_items);
    if let Some(source) = pressure_source {
        provenance.push(source);
    }

    let (atlas, atlas_source) = load_atlas(workspace.as_deref(), repository_id.as_deref());
    if let Some(source) = atlas_source {
        provenance.push(source);
    }

    if local_repository.is_none() {
        limitations.push(
            "local .mncs/project.json is absent; repository ownership and contracts are UNKNOWN"
                .to_owned(),
        );
    }
    let authoritative_complete = local_repository.is_some()
        && language.complete
        && architecture.complete
        && limitations.is_empty();
    let bounded = language.complete && architecture.complete;
    let state = if authoritative_complete {
        "complete"
    } else if bounded {
        "partial"
    } else {
        "unknown"
    };

    Ok(FamilyAgentContextResponse {
        schema_version: FAMILY_CONTEXT_SCHEMA.to_owned(),
        status: ResponseStatus::Answered,
        repository: local_repository,
        language,
        architecture,
        atlas,
        pressures,
        completeness: ContextCompleteness {
            complete: authoritative_complete,
            state: state.to_owned(),
            limitations,
        },
        provenance,
    })
}

fn load_repository_manifest(
    workspace: Option<&Path>,
) -> (
    Option<RepositoryContext>,
    Vec<String>,
    Option<ContextSource>,
) {
    let Some(root) = workspace else {
        return (None, vec!["workspace root is unavailable".to_owned()], None);
    };
    let path = root.join(".mncs/project.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return (None, Vec::new(), None),
    };
    let identity = sha256(&bytes);
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return (
                None,
                vec![format!("local manifest is invalid JSON: {error}")],
                Some(ContextSource {
                    authority: "repository".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    identity: Some(identity),
                }),
            )
        }
    };
    let repository = value
        .get("repository")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let revision = value.get("revision").and_then(Value::as_u64);
    let authority = value.get("authority").cloned();
    let contracts = value.get("contracts").cloned();
    let mut limitations = Vec::new();
    if value.get("schema_version").and_then(Value::as_str)
        != Some("mncs-family.repository-manifest/v0alpha1")
    {
        limitations.push("local manifest has an unsupported schema_version".to_owned());
    }
    if repository.is_none() || revision.is_none() || contracts.is_none() {
        limitations.push(
            "local manifest is missing repository, revision, or contracts required by the family manifest contract"
                .to_owned(),
        );
    }
    (
        Some(RepositoryContext {
            repository,
            revision,
            manifest_path: path.to_string_lossy().into_owned(),
            manifest_identity: identity.clone(),
            authority,
            contracts,
            present: true,
        }),
        limitations,
        Some(ContextSource {
            authority: "repository".to_owned(),
            path: path.to_string_lossy().into_owned(),
            identity: Some(identity),
        }),
    )
}

fn discover_commons_root(workspace: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("MNCS_COMMONS_ROOT") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(root) = workspace {
        candidates.push(root.to_path_buf());
        candidates.push(root.join("MNCS-Commons"));
        let mut ancestor = root.parent();
        for _ in 0..4 {
            let Some(path) = ancestor else { break };
            candidates.push(path.join("MNCS-Commons"));
            ancestor = path.parent();
        }
    }
    if let Ok(current) = env::current_dir() {
        candidates.push(current.join("MNCS-Commons"));
    }
    let mut seen = BTreeSet::new();
    candidates.into_iter().find(|candidate| {
        seen.insert(candidate.clone())
            && candidate
                .join("family/architecture-model-v1.json")
                .is_file()
    })
}

fn load_architecture(
    commons_root: Option<&Path>,
    repository: Option<&str>,
    known_identity: Option<&str>,
    max_items: usize,
) -> (ArchitectureContext, Vec<String>, Vec<ContextSource>) {
    let Some(root) = commons_root else {
        return (
            ArchitectureContext {
                source_path: None,
                schema_identity: None,
                content_identity: None,
                mode: "unknown".to_owned(),
                capabilities: Vec::new(),
                generators: Vec::new(),
                delta: None,
                complete: false,
                limitations: vec!["Commons architecture model was not found".to_owned()],
            },
            vec!["Commons architecture projection unavailable".to_owned()],
            Vec::new(),
        );
    };
    let path = root.join("family/architecture-model-v1.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                ArchitectureContext {
                    source_path: Some(path.to_string_lossy().into_owned()),
                    schema_identity: None,
                    content_identity: None,
                    mode: "unknown".to_owned(),
                    capabilities: Vec::new(),
                    generators: Vec::new(),
                    delta: None,
                    complete: false,
                    limitations: vec![error.to_string()],
                },
                vec!["Commons architecture projection could not be read".to_owned()],
                Vec::new(),
            );
        }
    };
    let model: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return (
                ArchitectureContext {
                    source_path: Some(path.to_string_lossy().into_owned()),
                    schema_identity: None,
                    content_identity: None,
                    mode: "unknown".to_owned(),
                    capabilities: Vec::new(),
                    generators: Vec::new(),
                    delta: None,
                    complete: false,
                    limitations: vec![format!("invalid Commons architecture JSON: {error}")],
                },
                vec!["Commons architecture projection is invalid".to_owned()],
                vec![ContextSource {
                    authority: "MNCS-Commons".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    identity: Some(sha256(&bytes)),
                }],
            );
        }
    };
    let schema_identity = model
        .get("schema_identity")
        .or_else(|| model.get("schema_version"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let content_identity = model
        .get("content_identity")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut limitations = Vec::new();
    if schema_identity.as_deref() != Some(ARCHITECTURE_SCHEMA) {
        limitations.push("Commons architecture schema identity is not supported".to_owned());
    }
    if content_identity.is_none() {
        limitations.push("Commons architecture content identity is missing".to_owned());
    }
    let current_capabilities = relevant_capabilities(&model, repository, None, max_items);
    let all_relevant_count = relevant_capabilities(&model, repository, None, usize::MAX).len();
    let current_generators = model
        .get("generators")
        .and_then(Value::as_array)
        .map(|items| items.iter().take(max_items).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let generator_count = model
        .get("generators")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if all_relevant_count > max_items || generator_count > max_items {
        limitations.push("architecture collections are bounded by max_items".to_owned());
    }
    let mut mode = "full".to_owned();
    let mut delta = None;
    let mut capabilities = current_capabilities;
    let mut generators = current_generators;
    if known_identity.is_some() && known_identity == content_identity.as_deref() {
        mode = "unchanged".to_owned();
        capabilities.clear();
        generators.clear();
    } else if let (Some(known), Some(current)) = (known_identity, content_identity.as_deref()) {
        if let Some(chain) = architecture_delta_chain(root, &model, known, current) {
            mode = "delta".to_owned();
            delta = Some(serde_json::json!({
                "from": known,
                "to": current,
                "mode": "delta",
                "chain": chain,
            }));
        } else {
            limitations.push(
                "requested architecture identity is outside the retained delta chain; current bounded projection returned"
                    .to_owned(),
            );
        }
    }
    if repository.is_none() {
        limitations.push(
            "repository identity is unavailable; architecture capabilities are not scoped"
                .to_owned(),
        );
    }
    let complete = !limitations.iter().any(|item| {
        item.contains("not supported")
            || item.contains("missing")
            || item.contains("unavailable")
            || item.contains("invalid")
            || item.contains("outside")
            || item.contains("bounded")
            || item.contains("unavailable")
    });
    let mut sources = vec![ContextSource {
        authority: "MNCS-Commons".to_owned(),
        path: path.to_string_lossy().into_owned(),
        identity: content_identity.clone(),
    }];
    if let Some(history_path) = delta_history_path(&model, root) {
        if let Ok(history_bytes) = fs::read(&history_path) {
            sources.push(ContextSource {
                authority: "MNCS-Commons".to_owned(),
                path: history_path.to_string_lossy().into_owned(),
                identity: Some(sha256(&history_bytes)),
            });
        }
    }
    (
        ArchitectureContext {
            source_path: Some(path.to_string_lossy().into_owned()),
            schema_identity,
            content_identity,
            mode,
            capabilities,
            generators,
            delta,
            complete,
            limitations,
        },
        Vec::new(),
        sources,
    )
}

fn relevant_capabilities(
    model: &Value,
    repository: Option<&str>,
    filter: Option<&str>,
    max_items: usize,
) -> Vec<Value> {
    let needle = filter.map(str::to_ascii_lowercase);
    let mut values = model
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    let owned = repository.is_some_and(|wanted| {
                        item.get("owner").and_then(Value::as_str) == Some(wanted)
                            || item
                                .get("canonical")
                                .and_then(|value| value.get("repository"))
                                .and_then(Value::as_str)
                                == Some(wanted)
                    });
                    let text = serde_json::to_string(item)
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let filtered = needle
                        .as_deref()
                        .map(|wanted| text.contains(wanted))
                        .unwrap_or(true);
                    owned && filtered
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    values.truncate(max_items);
    values
}

fn delta_history_path(model: &Value, root: &Path) -> Option<PathBuf> {
    let relative = model
        .get("delta_history")
        .and_then(|value| value.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("family/architecture-delta-history-v1.json");
    let path = root.join(relative);
    path.is_file().then_some(path)
}

fn architecture_delta_chain(
    root: &Path,
    model: &Value,
    known: &str,
    current: &str,
) -> Option<Vec<Value>> {
    let path = delta_history_path(model, root)?;
    let history: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let deltas = history.get("deltas")?.as_array()?;
    let mut chain = Vec::new();
    let mut cursor = current.to_owned();
    while cursor != known {
        let item = deltas.iter().rev().find(|item| {
            item.get("current_content_identity").and_then(Value::as_str) == Some(cursor.as_str())
        })?;
        chain.push(item.clone());
        cursor = item
            .get("previous_content_identity")
            .and_then(Value::as_str)?
            .to_owned();
        if chain.len() > deltas.len() {
            return None;
        }
    }
    chain.reverse();
    Some(chain)
}

fn load_pressures(
    commons_root: Option<&Path>,
    repository: Option<&str>,
    max_items: usize,
) -> (Vec<PressureSummary>, Option<ContextSource>) {
    let (Some(root), Some(repository)) = (commons_root, repository) else {
        return (Vec::new(), None);
    };
    let view_path = root.join("pressures/views/unresolved-language.json");
    if let Ok(bytes) = fs::read(&view_path) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            let mut rows = value
                .get("pressures")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| {
                            item.get("affectedRepositories")
                                .and_then(Value::as_array)
                                .is_some_and(|repos| {
                                    repos
                                        .iter()
                                        .any(|candidate| candidate.as_str() == Some(repository))
                                })
                        })
                        .filter_map(|item| pressure_summary(item, &view_path))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            rows.sort_by(|left, right| left.id.cmp(&right.id));
            rows.truncate(max_items);
            return (
                rows,
                Some(ContextSource {
                    authority: "MNCS-Commons".to_owned(),
                    path: view_path.to_string_lossy().into_owned(),
                    identity: Some(sha256(&bytes)),
                }),
            );
        }
    }
    (Vec::new(), None)
}

fn pressure_summary(value: &Value, source: &Path) -> Option<PressureSummary> {
    let id = value.get("id")?.as_str()?.to_owned();
    let required_behavior = source
        .parent()
        .and_then(Path::parent)
        .map(|pressures| pressures.join("records").join(format!("{id}.json")))
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|record| {
            record
                .get("requiredBehavior")
                .or_else(|| record.get("required_behavior"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Some(PressureSummary {
        id,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        target: value
            .get("target")
            .and_then(Value::as_str)
            .map(str::to_owned),
        domain: value
            .get("domain")
            .and_then(Value::as_str)
            .map(str::to_owned),
        severity: value
            .get("severity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        verification_state: value
            .get("verificationState")
            .and_then(Value::as_str)
            .map(str::to_owned),
        unresolved: value.get("unresolved").and_then(Value::as_bool),
        affected_repositories: value
            .get("affectedRepositories")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        required_behavior,
        source_path: source.to_string_lossy().into_owned(),
    })
}

fn load_atlas(
    workspace: Option<&Path>,
    repository: Option<&str>,
) -> (Option<AtlasContext>, Option<ContextSource>) {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("MNCS_ATLAS_ROOT") {
        candidates.push(PathBuf::from(path).join("registry/compiled.json"));
    }
    if let Some(root) = workspace {
        candidates.push(root.join("registry/compiled.json"));
        candidates.push(root.join("mncs-atlas/registry/compiled.json"));
        let mut ancestor = root.parent();
        for _ in 0..4 {
            let Some(path) = ancestor else { break };
            candidates.push(path.join("mncs-atlas/registry/compiled.json"));
            ancestor = path.parent();
        }
    }
    let Some(path) = candidates.into_iter().find(|candidate| candidate.is_file()) else {
        return (None, None);
    };
    let Ok(bytes) = fs::read(&path) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return (None, None);
    };
    let project = repository.and_then(|wanted| {
        value
            .get("projects")
            .and_then(Value::as_array)
            .and_then(|projects| {
                projects
                    .iter()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(wanted))
            })
            .map(|item| {
                let mut compact = serde_json::Map::new();
                for key in [
                    "id",
                    "role",
                    "category",
                    "lifecycle",
                    "authority_class",
                    "responsibility",
                    "dependencies",
                    "language_profile_status",
                ] {
                    if let Some(value) = item.get(key) {
                        compact.insert(key.to_owned(), value.clone());
                    }
                }
                Value::Object(compact)
            })
    });
    let context = AtlasContext {
        source_path: path.to_string_lossy().into_owned(),
        registry_identity: value
            .get("registry_hash")
            .and_then(Value::as_str)
            .map(|hash| format!("sha256:{hash}")),
        registry_revision: value
            .get("registry_revision")
            .and_then(Value::as_str)
            .map(str::to_owned),
        authority: "orientation-only; owning repositories and Commons retain authority".to_owned(),
        project,
    };
    (
        Some(context),
        Some(ContextSource {
            authority: "mncs-atlas (non-normative orientation)".to_owned(),
            path: path.to_string_lossy().into_owned(),
            identity: Some(sha256(&bytes)),
        }),
    )
}

fn sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("sha256:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "mncs-family-context-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".mncs")).unwrap();
        fs::create_dir_all(root.join("MNCS-Commons/family")).unwrap();
        fs::create_dir_all(root.join("MNCS-Commons/pressures/views")).unwrap();
        fs::create_dir_all(root.join("MNCS-Commons/pressures/records")).unwrap();
        fs::create_dir_all(root.join("mncs-language/docs")).unwrap();
        root
    }

    fn write_fixtures(root: &Path) {
        fs::write(
            root.join(".mncs/project.json"),
            r#"{"schema_version":"mncs-family.repository-manifest/v0alpha1","repository":"fixture-repo","revision":1,"contracts":{"provides":[],"consumes":[],"tests":[]}}"#,
        )
        .unwrap();
        fs::write(
            root.join("mncs-language/docs/language-capabilities.json"),
            serde_json::json!({
                "schema_version":"mncs.language-capabilities/1",
                "generator_identity":"fixture",
                "language":"MNCS",
                "current_profile":"0.18",
                "profile_registry_identity":"fixture-profile",
                "compiler_inventory_identity":"fixture-inventory",
                "profiles":[],"library_modules":[],"intrinsics":[],"topics":[],"examples":[],"provenance":[{"path":"fixture","kind":"fixture","source_identity":"fixture"}],"projections":{},"capsule":{},"content_identity":"lang-current"
            }).to_string(),
        )
        .unwrap();
        fs::write(
            root.join("mncs-language/docs/language-capability-deltas.json"),
            serde_json::json!({
                "schema_version": "mncs.language-capability-deltas/1",
                "retention": 8,
                "history_identity": "fixture-history",
                "deltas": [{
                    "previous_content_identity": "lang-old",
                    "current_content_identity": "lang-current",
                    "from_profile": "0.17",
                    "to_profile": "0.18",
                    "modules": {"added": ["fixture.module"]}
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join("MNCS-Commons/family/architecture-model-v1.json"),
            serde_json::json!({
                "schema_version":ARCHITECTURE_SCHEMA,
                "schema_identity":ARCHITECTURE_SCHEMA,
                "content_identity":"arch-current",
                "capabilities":[{"id":"fixture.capability","owner":"fixture-repo","canonical":{"repository":"fixture-repo","path":"native/main.mncs"},"active_alternates":[]}],
                "generators":[]
            }).to_string(),
        ).unwrap();
        fs::write(
            root.join("MNCS-Commons/family/architecture-delta-history-v1.json"),
            serde_json::json!({
                "schema_version": "commons.mncs.architecture-delta-history/1",
                "retention": 8,
                "deltas": [{
                    "previous_content_identity": "arch-old",
                    "current_content_identity": "arch-current",
                    "changed_capabilities": ["fixture.capability"],
                    "added_contracts": [],
                    "removed_contracts": [],
                    "ownership_changes": [],
                    "shadow_state_transitions": []
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join("MNCS-Commons/pressures/views/unresolved-language.json"),
            serde_json::json!({"pressures":[{"id":"FIXTURE-P","title":"fixture","target":"language","affectedRepositories":["fixture-repo"],"unresolved":true}]}).to_string(),
        ).unwrap();
        fs::write(
            root.join("MNCS-Commons/pressures/records/FIXTURE-P.json"),
            serde_json::json!({"id":"FIXTURE-P","requiredBehavior":"fixture behavior"}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn current_identity_is_bounded_and_complete() {
        let root = temp_root();
        write_fixtures(&root);
        let response = query(
            Some(&root),
            None,
            None,
            None,
            Some("lang-current"),
            Some("arch-current"),
            8,
        )
        .unwrap();
        assert_eq!(response.schema_version, FAMILY_CONTEXT_SCHEMA);
        assert_eq!(response.language.mode, "unchanged");
        assert_eq!(response.architecture.mode, "unchanged");
        assert!(response.completeness.complete);
        assert_eq!(response.pressures.len(), 1);
        assert_eq!(response.pressures[0].id, "FIXTURE-P");
        assert_eq!(
            response.pressures[0].required_behavior.as_deref(),
            Some("fixture behavior")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_identities_return_bounded_deltas() {
        let root = temp_root();
        write_fixtures(&root);
        let response = query(
            Some(&root),
            None,
            None,
            None,
            Some("lang-old"),
            Some("arch-old"),
            8,
        )
        .unwrap();
        assert_eq!(response.language.mode, "delta");
        assert_eq!(response.architecture.mode, "delta");
        assert!(response.language.delta.is_some());
        assert!(response.architecture.delta.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_authorities_remain_unknown() {
        let root = temp_root();
        fs::write(
            root.join(".mncs/project.json"),
            r#"{"schema_version":"mncs-family.repository-manifest/v0alpha1","repository":"fixture-repo","revision":1,"contracts":{"provides":[],"consumes":[],"tests":[]}}"#,
        )
        .unwrap();
        let response = query(Some(&root), None, None, None, None, None, 8).unwrap();
        assert_eq!(response.completeness.state, "unknown");
        assert!(!response.completeness.complete);
        fs::remove_dir_all(root).unwrap();
    }
}
