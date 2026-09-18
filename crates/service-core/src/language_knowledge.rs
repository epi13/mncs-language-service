//! Query adapter for the authoritative `mncs-language` capability index.
//!
//! Language facts are generated and revision-addressed by `mncs-language`.
//! This crate adds bounded topic/symbol/profile filtering and delta selection;
//! it does not maintain a second language reference.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ServiceError;

const SCHEMA: &str = "mncs.language-capabilities/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageModule {
    pub module: Option<String>,
    pub profile: Option<String>,
    pub path: String,
    pub source_identity: String,
    #[serde(default)]
    pub symbols: Vec<LanguageSymbol>,
    #[serde(default)]
    pub exports: Vec<String>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub effects: Vec<LanguageEffect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageSymbol {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageEffect {
    pub capability: String,
    pub effect: String,
    pub authorized_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageTopic {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default)]
    pub guidance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageProvenance {
    pub path: String,
    pub kind: String,
    pub source_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCapabilityIndex {
    pub schema_version: String,
    pub generator_identity: String,
    pub language: String,
    pub current_profile: String,
    pub profile_registry_identity: String,
    #[serde(default)]
    pub profiles: Vec<serde_json::Value>,
    #[serde(default)]
    pub library_modules: Vec<LanguageModule>,
    #[serde(default)]
    pub intrinsics: Vec<serde_json::Value>,
    #[serde(default)]
    pub topics: Vec<LanguageTopic>,
    #[serde(default)]
    pub examples: Vec<serde_json::Value>,
    #[serde(default)]
    pub provenance: Vec<LanguageProvenance>,
    pub projections: serde_json::Value,
    pub capsule: serde_json::Value,
    pub content_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageDelta {
    pub from_profile: String,
    pub to_profile: String,
    pub profiles: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageProjection {
    pub mode: String,
    pub layers: Vec<String>,
    pub counts: BTreeMap<String, usize>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCapabilitiesResponse {
    pub schema_version: String,
    pub source_path: String,
    pub content_identity: String,
    pub current_profile: String,
    pub capsule: serde_json::Value,
    pub topics: Vec<LanguageTopic>,
    pub modules: Vec<LanguageModule>,
    pub examples: Vec<serde_json::Value>,
    pub intrinsics: Vec<serde_json::Value>,
    pub provenance: Vec<LanguageProvenance>,
    pub projection: LanguageProjection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<LanguageDelta>,
}

pub fn discover(root: Option<&Path>) -> Result<(PathBuf, LanguageCapabilityIndex), ServiceError> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("MNCS_LANGUAGE_CAPABILITY_INDEX") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("MNCS_LANGUAGE_ROOT") {
        candidates.push(PathBuf::from(path).join("docs/language-capabilities.json"));
    }
    if let Some(root) = root {
        candidates.push(root.join("docs/language-capabilities.json"));
        if let Some(parent) = root.parent() {
            candidates.push(parent.join("mncs-language/docs/language-capabilities.json"));
        }
    }
    if let Ok(current) = env::current_dir() {
        candidates.push(current.join("docs/language-capabilities.json"));
        if let Some(parent) = current.parent() {
            candidates.push(parent.join("mncs-language/docs/language-capabilities.json"));
        }
    }
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();
    for path in candidates {
        if !seen.insert(path.clone()) || !path.is_file() {
            continue;
        }
        match load(&path) {
            Ok(index) => return Ok((path, index)),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    let reason = if errors.is_empty() {
        "authoritative language capability index was not found".to_owned()
    } else {
        format!(
            "authoritative language capability index is invalid ({})",
            errors.join("; ")
        )
    };
    Err(ServiceError::Unsupported { reason })
}

pub fn load(path: &Path) -> Result<LanguageCapabilityIndex, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let index: LanguageCapabilityIndex =
        serde_json::from_str(&text).map_err(|error| error.to_string())?;
    if index.schema_version != SCHEMA {
        return Err(format!("expected {SCHEMA}, found {}", index.schema_version));
    }
    if index.language != "MNCS"
        || index.current_profile.is_empty()
        || index.content_identity.is_empty()
        || index.profile_registry_identity.is_empty()
    {
        return Err("index envelope is incomplete".to_owned());
    }
    Ok(index)
}

pub fn query(
    root: Option<&Path>,
    topic: Option<&str>,
    symbol: Option<&str>,
    profile: Option<&str>,
    delta_from: Option<&str>,
    known_identity: Option<&str>,
    max_items: usize,
) -> Result<LanguageCapabilitiesResponse, ServiceError> {
    if max_items == 0 {
        return Err(ServiceError::InvalidRequest {
            reason: "max_items must be greater than zero".to_owned(),
        });
    }
    let (path, index) = discover(root)?;
    if known_identity == Some(index.content_identity.as_str()) {
        let mut counts = BTreeMap::new();
        for key in [
            "topics",
            "modules",
            "symbols",
            "effects",
            "examples",
            "intrinsics",
            "provenance",
            "profiles",
        ] {
            counts.insert(key.to_owned(), 0);
        }
        return Ok(LanguageCapabilitiesResponse {
            schema_version: SCHEMA.to_owned(),
            source_path: path.to_string_lossy().into_owned(),
            content_identity: index.content_identity,
            current_profile: index.current_profile,
            capsule: serde_json::Value::Object(Default::default()),
            topics: Vec::new(),
            modules: Vec::new(),
            examples: Vec::new(),
            intrinsics: Vec::new(),
            provenance: Vec::new(),
            projection: LanguageProjection {
                mode: "unchanged".to_owned(),
                layers: vec!["identity".to_owned()],
                counts,
                complete: true,
                limitations: Vec::new(),
            },
            delta: None,
        });
    }
    let topic_filter = topic.map(str::to_ascii_lowercase);
    let symbol_filter = symbol.map(str::to_ascii_lowercase);
    let profile_filter = profile.map(str::to_owned);
    let filtered_topics = index
        .topics
        .iter()
        .filter(|item| {
            topic_filter
                .as_deref()
                .map(|needle| item.id.to_ascii_lowercase().contains(needle))
                .unwrap_or(true)
        })
        .cloned()
        .collect::<Vec<_>>();
    let filtered_modules = index
        .library_modules
        .iter()
        .filter(|item| {
            let matches_symbol = symbol_filter.as_deref().map_or(true, |needle| {
                item.module
                    .as_deref()
                    .map(|module| module.to_ascii_lowercase().contains(needle))
                    .unwrap_or(false)
                    || item
                        .symbols
                        .iter()
                        .any(|symbol| symbol.name.to_ascii_lowercase().contains(needle))
            });
            let matches_profile = profile_filter
                .as_deref()
                .map(|wanted| item.profile.as_deref() == Some(wanted))
                .unwrap_or(true);
            matches_symbol && matches_profile
        })
        .cloned()
        .collect::<Vec<_>>();
    let topics = filtered_topics
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let modules = filtered_modules
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let examples = index
        .examples
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let intrinsics = index
        .intrinsics
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let provenance = index
        .provenance
        .iter()
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let delta_profiles = delta_from.map(|from| {
        index
            .profiles
            .iter()
            .filter(|profile| {
                profile
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(|value| version_gt(value, from))
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>()
    });
    let delta = delta_from.map(|from| LanguageDelta {
        from_profile: from.to_owned(),
        to_profile: index.current_profile.clone(),
        profiles: delta_profiles
            .as_ref()
            .expect("delta profiles are present when delta_from is set")
            .iter()
            .take(max_items)
            .cloned()
            .collect(),
    });
    let truncated = filtered_topics.len() > max_items
        || filtered_modules.len() > max_items
        || index.examples.len() > max_items
        || index.intrinsics.len() > max_items
        || index.provenance.len() > max_items
        || delta_profiles
            .as_ref()
            .is_some_and(|items| items.len() > max_items);
    let mut counts = BTreeMap::new();
    counts.insert("topics".to_owned(), topics.len());
    counts.insert("modules".to_owned(), modules.len());
    counts.insert(
        "symbols".to_owned(),
        modules.iter().map(|module| module.symbols.len()).sum(),
    );
    counts.insert(
        "effects".to_owned(),
        modules.iter().map(|module| module.effects.len()).sum(),
    );
    counts.insert("examples".to_owned(), examples.len());
    counts.insert("intrinsics".to_owned(), intrinsics.len());
    counts.insert("provenance".to_owned(), provenance.len());
    counts.insert(
        "profiles".to_owned(),
        delta.as_ref().map(|item| item.profiles.len()).unwrap_or(0),
    );
    let mut limitations = Vec::new();
    if truncated {
        limitations.push("one or more collections are bounded by max_items".to_owned());
    }
    Ok(LanguageCapabilitiesResponse {
        schema_version: SCHEMA.to_owned(),
        source_path: path.to_string_lossy().into_owned(),
        content_identity: index.content_identity,
        current_profile: index.current_profile,
        capsule: index.capsule,
        topics,
        modules,
        examples,
        intrinsics,
        provenance,
        projection: LanguageProjection {
            mode: if delta_from.is_some() {
                "delta".to_owned()
            } else {
                "full".to_owned()
            },
            layers: {
                let mut layers = vec![
                    "identity".to_owned(),
                    "implementation".to_owned(),
                    "evidence".to_owned(),
                ];
                if delta_from.is_some() {
                    layers.push("delta".to_owned());
                }
                layers
            },
            counts,
            complete: !truncated,
            limitations,
        },
        delta,
    })
}

fn version_gt(left: &str, right: &str) -> bool {
    fn parts(value: &str) -> (u32, u32) {
        let mut values = value
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0));
        (values.next().unwrap_or(0), values.next().unwrap_or(0))
    }
    parts(left) > parts(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mncs-language-service-capabilities-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("docs")).expect("fixture directory");
        let index = LanguageCapabilityIndex {
            schema_version: SCHEMA.to_owned(),
            generator_identity: "fixture".to_owned(),
            language: "MNCS".to_owned(),
            current_profile: "0.18".to_owned(),
            profile_registry_identity: "fixture-profile".to_owned(),
            profiles: vec![
                serde_json::json!({"version": "0.18"}),
                serde_json::json!({"version": "0.19"}),
            ],
            library_modules: vec![LanguageModule {
                module: Some("mncs.fixture".to_owned()),
                profile: Some("0.18".to_owned()),
                path: "fixture.mncs".to_owned(),
                source_identity: "fixture-source".to_owned(),
                symbols: vec![LanguageSymbol {
                    kind: "function".to_owned(),
                    name: "answer".to_owned(),
                }],
                exports: vec!["answer".to_owned()],
                imports: vec!["mncs.fixture.dep".to_owned()],
                capabilities: vec!["fixture_capability".to_owned()],
                effects: vec![LanguageEffect {
                    capability: "fixture_capability".to_owned(),
                    effect: "fixture_effect".to_owned(),
                    authorized_by: "fixture_capability".to_owned(),
                }],
            }],
            intrinsics: vec![
                serde_json::json!({"id": "one"}),
                serde_json::json!({"id": "two"}),
            ],
            topics: vec![
                LanguageTopic {
                    id: "identity".to_owned(),
                    description: "identity".to_owned(),
                    profiles: vec!["0.18".to_owned()],
                    modules: vec!["mncs.fixture".to_owned()],
                    guidance: vec![],
                },
                LanguageTopic {
                    id: "effects".to_owned(),
                    description: "effects".to_owned(),
                    profiles: vec!["0.18".to_owned()],
                    modules: vec!["mncs.fixture".to_owned()],
                    guidance: vec![],
                },
            ],
            examples: vec![
                serde_json::json!({"id": "one"}),
                serde_json::json!({"id": "two"}),
            ],
            provenance: vec![LanguageProvenance {
                path: "fixture.mncs".to_owned(),
                kind: "fixture".to_owned(),
                source_identity: "fixture-source".to_owned(),
            }],
            projections: serde_json::json!({}),
            capsule: serde_json::json!({"profile": "0.18"}),
            content_identity: "fixture-content".to_owned(),
        };
        fs::write(
            root.join("docs/language-capabilities.json"),
            serde_json::to_vec(&index).expect("fixture json"),
        )
        .expect("fixture index");
        root
    }

    #[test]
    fn profile_delta_is_numeric_and_not_lexical() {
        assert!(version_gt("0.18", "0.9"));
        assert!(!version_gt("0.8", "0.18"));
    }

    #[test]
    fn invalid_max_items_is_rejected_before_discovery() {
        let error = query(None, None, None, None, None, None, 0).unwrap_err();
        assert!(matches!(error, ServiceError::InvalidRequest { .. }));
    }

    #[test]
    fn targeted_projection_is_smaller_and_identity_can_be_unchanged() {
        let root = fixture_root();
        let full = query(Some(&root), None, None, None, None, None, 16).expect("full query");
        let targeted = query(
            Some(&root),
            Some("identity"),
            Some("answer"),
            Some("0.18"),
            None,
            None,
            1,
        )
        .expect("targeted query");
        let unchanged = query(
            Some(&root),
            None,
            None,
            None,
            None,
            Some("fixture-content"),
            16,
        )
        .expect("unchanged query");
        assert!(
            serde_json::to_vec(&full).unwrap().len() > serde_json::to_vec(&targeted).unwrap().len()
        );
        assert_eq!(targeted.projection.mode, "full");
        assert_eq!(targeted.projection.counts["symbols"], 1);
        assert_eq!(targeted.projection.counts["effects"], 1);
        assert_eq!(unchanged.projection.mode, "unchanged");
        assert!(unchanged.modules.is_empty());
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
