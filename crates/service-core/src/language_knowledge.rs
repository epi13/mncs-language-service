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
const DELTA_SCHEMA: &str = "mncs.language-capability-deltas/1";

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
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_content_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_content_identity: Option<String>,
    pub from_profile: String,
    pub to_profile: String,
    pub profiles: Vec<serde_json::Value>,
    #[serde(default)]
    pub profile_changes: LanguageChangeSet,
    #[serde(default)]
    pub modules: LanguageChangeSet,
    #[serde(default)]
    pub exports: LanguageChangeSet,
    #[serde(default)]
    pub intrinsics: LanguageChangeSet,
    #[serde(default)]
    pub effects: LanguageChangeSet,
    #[serde(default)]
    pub capabilities: LanguageChangeSet,
    #[serde(default)]
    pub canonical_examples: LanguageChangeSet,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanguageChangeSet {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LanguageDeltaHistory {
    schema_version: String,
    #[allow(dead_code)]
    retention: usize,
    #[allow(dead_code)]
    history_identity: String,
    #[serde(default)]
    deltas: Vec<LanguageDeltaEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct LanguageDeltaEntry {
    previous_content_identity: String,
    current_content_identity: String,
    #[serde(default)]
    from_profile: Option<String>,
    #[serde(default)]
    to_profile: Option<String>,
    #[serde(default)]
    profiles: RawChangeSet<serde_json::Value>,
    #[serde(default)]
    modules: RawChangeSet<String>,
    #[serde(default)]
    exports: RawChangeSet<String>,
    #[serde(default)]
    intrinsics: RawChangeSet<String>,
    #[serde(default)]
    effects: RawChangeSet<String>,
    #[serde(default)]
    capabilities: RawChangeSet<String>,
    #[serde(default)]
    canonical_examples: RawChangeSet<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawChangeSet<T> {
    #[serde(default)]
    added: Vec<T>,
    #[serde(default)]
    changed: Vec<T>,
    #[serde(default)]
    removed: Vec<T>,
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

fn load_delta_history(path: &Path) -> Option<LanguageDeltaHistory> {
    let history_path = path.with_file_name("language-capability-deltas.json");
    let text = fs::read_to_string(history_path).ok()?;
    let history: LanguageDeltaHistory = serde_json::from_str(&text).ok()?;
    if history.schema_version != DELTA_SCHEMA
        || history.history_identity.is_empty()
        || history.retention == 0
        || history.deltas.len() > history.retention
    {
        return None;
    }
    Some(history)
}

fn merge_strings(target: &mut Vec<String>, values: &[String]) {
    target.extend(values.iter().cloned());
    target.sort();
    target.dedup();
}

fn merge_change_set(target: &mut LanguageChangeSet, source: &RawChangeSet<String>) {
    merge_strings(&mut target.added, &source.added);
    merge_strings(&mut target.changed, &source.changed);
    merge_strings(&mut target.removed, &source.removed);
}

fn merge_profile_changes(target: &mut LanguageChangeSet, source: &RawChangeSet<serde_json::Value>) {
    let name = |value: &serde_json::Value| {
        value
            .get("version")
            .and_then(|item| item.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    };
    for value in &source.added {
        target.added.push(name(value));
    }
    for value in &source.changed {
        target.changed.push(name(value));
    }
    for value in &source.removed {
        target.removed.push(name(value));
    }
    target.added.sort();
    target.added.dedup();
    target.changed.sort();
    target.changed.dedup();
    target.removed.sort();
    target.removed.dedup();
}

fn identity_delta(
    path: &Path,
    index: &LanguageCapabilityIndex,
    from: &str,
    max_items: usize,
) -> Option<LanguageDelta> {
    let history = load_delta_history(path)?;
    let mut chain = Vec::new();
    let mut cursor = index.content_identity.clone();
    while cursor != from {
        let item = history
            .deltas
            .iter()
            .rev()
            .find(|delta| delta.current_content_identity == cursor)?;
        chain.push(item.clone());
        cursor = item.previous_content_identity.clone();
        if chain.len() > history.retention {
            return None;
        }
    }
    if chain.is_empty() {
        return None;
    }
    chain.reverse();
    let mut delta = LanguageDelta {
        mode: "identity".to_owned(),
        from_content_identity: Some(from.to_owned()),
        to_content_identity: Some(index.content_identity.clone()),
        from_profile: String::new(),
        to_profile: index.current_profile.clone(),
        profiles: Vec::new(),
        profile_changes: LanguageChangeSet::default(),
        modules: LanguageChangeSet::default(),
        exports: LanguageChangeSet::default(),
        intrinsics: LanguageChangeSet::default(),
        effects: LanguageChangeSet::default(),
        capabilities: LanguageChangeSet::default(),
        canonical_examples: LanguageChangeSet::default(),
    };
    for item in chain {
        if delta.from_profile.is_empty() {
            delta.from_profile = item
                .from_profile
                .clone()
                .unwrap_or_else(|| index.current_profile.clone());
        }
        if let Some(to_profile) = &item.to_profile {
            delta.to_profile = to_profile.clone();
        }
        delta.profiles.extend(item.profiles.added.iter().cloned());
        merge_profile_changes(&mut delta.profile_changes, &item.profiles);
        merge_change_set(&mut delta.modules, &item.modules);
        merge_change_set(&mut delta.exports, &item.exports);
        merge_change_set(&mut delta.intrinsics, &item.intrinsics);
        merge_change_set(&mut delta.effects, &item.effects);
        merge_change_set(&mut delta.capabilities, &item.capabilities);
        merge_change_set(&mut delta.canonical_examples, &item.canonical_examples);
    }
    if delta.from_profile.is_empty() {
        delta.from_profile = index.current_profile.clone();
    }
    delta.profiles.truncate(max_items);
    for changes in [
        &mut delta.profile_changes,
        &mut delta.modules,
        &mut delta.exports,
        &mut delta.intrinsics,
        &mut delta.effects,
        &mut delta.capabilities,
        &mut delta.canonical_examples,
    ] {
        changes.added.truncate(max_items);
        changes.changed.truncate(max_items);
        changes.removed.truncate(max_items);
    }
    Some(delta)
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
    if let Some(from) = known_identity {
        if let Some(delta) = identity_delta(&path, &index, from, max_items) {
            let mut counts = BTreeMap::new();
            counts.insert("profiles".to_owned(), delta.profiles.len());
            counts.insert("modules".to_owned(), change_count(&delta.modules));
            counts.insert("exports".to_owned(), change_count(&delta.exports));
            counts.insert("intrinsics".to_owned(), change_count(&delta.intrinsics));
            counts.insert("effects".to_owned(), change_count(&delta.effects));
            counts.insert("capabilities".to_owned(), change_count(&delta.capabilities));
            counts.insert(
                "canonical_examples".to_owned(),
                change_count(&delta.canonical_examples),
            );
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
                    mode: "delta".to_owned(),
                    layers: vec!["identity".to_owned(), "delta".to_owned()],
                    counts,
                    complete: true,
                    limitations: Vec::new(),
                },
                delta: Some(delta),
            });
        }
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
        mode: "profile".to_owned(),
        from_content_identity: None,
        to_content_identity: None,
        from_profile: from.to_owned(),
        to_profile: index.current_profile.clone(),
        profiles: delta_profiles
            .as_ref()
            .expect("delta profiles are present when delta_from is set")
            .iter()
            .take(max_items)
            .cloned()
            .collect(),
        profile_changes: LanguageChangeSet::default(),
        modules: LanguageChangeSet::default(),
        exports: LanguageChangeSet::default(),
        intrinsics: LanguageChangeSet::default(),
        effects: LanguageChangeSet::default(),
        capabilities: LanguageChangeSet::default(),
        canonical_examples: LanguageChangeSet::default(),
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

fn change_count(changes: &LanguageChangeSet) -> usize {
    changes.added.len() + changes.changed.len() + changes.removed.len()
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
        fs::write(
            root.join("docs/language-capability-deltas.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": DELTA_SCHEMA,
                "retention": 8,
                "history_identity": "fixture-history",
                "deltas": [{
                    "previous_content_identity": "old-content",
                    "current_content_identity": "fixture-content",
                    "profiles": {"added": [{"version": "0.18"}], "changed": [], "removed": []},
                    "modules": {"added": ["mncs.fixture"], "changed": [], "removed": []},
                    "exports": {"added": ["mncs.fixture::answer"], "changed": [], "removed": []},
                    "intrinsics": {"added": ["select"], "changed": [], "removed": []},
                    "effects": {"added": ["clock_read"], "changed": [], "removed": []},
                    "capabilities": {"added": ["clock_grant"], "changed": [], "removed": []},
                    "canonical_examples": {"added": ["mncs.example/identity/1"], "changed": [], "removed": []}
                }]
            }))
            .expect("delta json"),
        )
        .expect("delta history");
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

    #[test]
    fn identity_delta_is_compact_and_semantic() {
        let root = fixture_root();
        let response = query(Some(&root), None, None, None, None, Some("old-content"), 16)
            .expect("identity delta");
        assert_eq!(response.projection.mode, "delta");
        let delta = response.delta.expect("delta payload");
        assert_eq!(delta.mode, "identity");
        assert_eq!(delta.from_content_identity.as_deref(), Some("old-content"));
        assert_eq!(delta.from_profile, "0.18");
        assert_eq!(delta.to_profile, "0.18");
        assert_eq!(delta.modules.added, vec!["mncs.fixture"]);
        assert_eq!(delta.intrinsics.added, vec!["select"]);
        assert!(response.modules.is_empty());
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
