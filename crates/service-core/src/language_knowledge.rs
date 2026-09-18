//! Query adapter for the authoritative `mncs-language` capability index.
//!
//! Language facts are generated and revision-addressed by `mncs-language`.
//! This crate adds bounded topic/symbol/profile filtering and delta selection;
//! it does not maintain a second language reference.

use std::collections::BTreeSet;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageSymbol {
    pub kind: String,
    pub name: String,
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
    max_items: usize,
) -> Result<LanguageCapabilitiesResponse, ServiceError> {
    if max_items == 0 {
        return Err(ServiceError::InvalidRequest {
            reason: "max_items must be greater than zero".to_owned(),
        });
    }
    let (path, index) = discover(root)?;
    let topic_filter = topic.map(str::to_ascii_lowercase);
    let symbol_filter = symbol.map(str::to_ascii_lowercase);
    let profile_filter = profile.map(str::to_owned);
    let topics = index
        .topics
        .iter()
        .filter(|item| {
            topic_filter
                .as_deref()
                .map(|needle| item.id.to_ascii_lowercase().contains(needle))
                .unwrap_or(true)
        })
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let modules = index
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
        .take(max_items)
        .cloned()
        .collect::<Vec<_>>();
    let examples = index.examples.iter().take(max_items).cloned().collect();
    let intrinsics = index.intrinsics.iter().take(max_items).cloned().collect();
    let provenance = index.provenance.iter().take(max_items).cloned().collect();
    let delta = delta_from.map(|from| LanguageDelta {
        from_profile: from.to_owned(),
        to_profile: index.current_profile.clone(),
        profiles: index
            .profiles
            .iter()
            .filter(|profile| {
                profile
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(|value| version_gt(value, from))
                    .unwrap_or(false)
            })
            .take(max_items)
            .cloned()
            .collect(),
    });
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

    #[test]
    fn profile_delta_is_numeric_and_not_lexical() {
        assert!(version_gt("0.18", "0.9"));
        assert!(!version_gt("0.8", "0.18"));
    }

    #[test]
    fn invalid_max_items_is_rejected_before_discovery() {
        let error = query(None, None, None, None, None, 0).unwrap_err();
        assert!(matches!(error, ServiceError::InvalidRequest { .. }));
    }
}
