//! Bounded family preflight composed from owning authority projections.
//!
//! Language Service owns the response envelope, protocol adaptation, and
//! bounds. The Standard validates repository manifests, `mncs-language`
//! supplies language/compiler facts, and Commons supplies architecture,
//! pressure lifecycle, filtering, and delta semantics through its bounded
//! `family agent-context` read surface. This module deliberately does not
//! read Commons' raw model, delta history, pressure records, or generated
//! views.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::language_knowledge::{
    self, LanguageChangeSet, LanguageDelta, LanguageModule, LanguageTopic,
};
use crate::{ResponseStatus, ServiceError};

pub const FAMILY_CONTEXT_SCHEMA: &str = "mncs.family-agent-context/2";
const COMMONS_PROJECTION_SCHEMA: &str = "commons.mncs.family-agent-projection/1";
const MANIFEST_VALIDATION_SCHEMA: &str = "mncs.standard.repository-manifest-validation/1";
const OBLIGATION_INVENTORY_SCHEMA: &str = "mncs-family.verification-obligation-inventory/v1";
const MAX_CONTEXT_ITEMS: usize = 32;
const LANGUAGE_AUTHORITY_ITEMS: usize = 256;
const AUTHORITY_QUERY_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_AUTHORITY_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyAgentContextResponse {
    pub schema_version: String,
    pub status: ResponseStatus,
    pub repository: Option<RepositoryContext>,
    pub language: LanguageContext,
    pub architecture: ArchitectureContext,
    /// Repository-owned verification obligations and bounded negative
    /// knowledge.  The service composes this view; it does not decide which
    /// obligations exist or promote evidence.
    pub verification: VerificationContext,
    /// Optional non-normative orientation. Atlas never contributes to
    /// `complete`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas: Option<AtlasContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pressures: Vec<PressureSummary>,
    #[serde(default)]
    pub negative_knowledge: Vec<NegativeKnowledge>,
    pub completeness: ContextCompleteness,
    pub provenance: Vec<ContextSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryContext {
    pub repository: Option<String>,
    pub revision: Option<u64>,
    pub manifest_path: String,
    pub manifest_identity: String,
    pub manifest_conformance_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_validation_identity: Option<String>,
    pub authority: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<Value>,
    pub contracts: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<Value>,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationContext {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<VerificationObligationSummary>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationObligationSummary {
    pub identity: String,
    pub title: String,
    pub guarantee_domain: String,
    pub evidence_role: String,
    pub lifecycle: String,
    pub scope: String,
    pub executor_provider: String,
    pub executor_kind: String,
    pub ordinary_verification: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegativeKnowledge {
    pub identity: String,
    pub disposition: String,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageContext {
    pub source_path: Option<String>,
    pub current_profile: Option<String>,
    pub content_identity: Option<String>,
    pub compiler_inventory_identity: Option<String>,
    pub mode: String,
    pub state: String,
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
    pub validation_identity: Option<String>,
    pub validation_state: String,
    pub freshness: String,
    pub projection_identity: Option<String>,
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
    pub state: String,
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
pub struct ContextAuthorityState {
    pub authority: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// A bounded projection may bind a second owning identity.  Commons
    /// pressure rows, for example, are only complete when both the generated
    /// view identity and the validated registry identity are current.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCompleteness {
    pub complete: bool,
    pub state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<ContextAuthorityState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSource {
    pub authority: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub state: String,
}

#[derive(Debug)]
struct CommonsProjection {
    architecture: ArchitectureContext,
    pressures: Vec<PressureSummary>,
    limitations: Vec<String>,
    sources: Vec<ContextSource>,
    architecture_state: String,
    pressure_state: String,
    pressure_registry_identity: Option<String>,
    pressure_view_identity: Option<String>,
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

    let (local_repository, mut limitations, manifest_source, manifest_state) =
        load_repository_manifest(workspace.as_deref());
    let (verification, mut negative_knowledge, verification_source) =
        load_verification_context(workspace.as_deref(), local_repository.as_ref(), max_items);
    if verification.state == "invalid" || verification.state == "unavailable" {
        limitations.extend(verification.limitations.clone());
    }
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
    if let Some(source) = verification_source {
        provenance.push(source);
    }

    let language = match language_knowledge::query(
        workspace.as_deref(),
        topic,
        symbol,
        None,
        None,
        known_language_identity,
        LANGUAGE_AUTHORITY_ITEMS,
    ) {
        Ok(response) => {
            let projection_limitations = response.projection.limitations.clone();
            let mut language_limitations = projection_limitations.clone();
            let mut topics = response.topics;
            let mut modules = response.modules;
            let mut delta = response.delta;
            let response_truncated = topics.len() > max_items
                || modules.len() > max_items
                || delta
                    .as_ref()
                    .is_some_and(|value| language_delta_has_more(value, max_items));
            topics.truncate(max_items);
            modules.truncate(max_items);
            if let Some(value) = &mut delta {
                bound_language_delta(value, max_items);
            }
            if response_truncated {
                language_limitations.push(
                    "language capability rows are bounded by the family context limit".to_owned(),
                );
            }
            // `response_truncated` describes Language Service's packet bound,
            // not a stale or unverified language authority.  The source
            // identity remains current and the authority projection itself
            // is complete at its bounded query limit.
            let complete =
                response.projection.complete && response.compiler_inventory_identity.is_some();
            if response.compiler_inventory_identity.is_none() {
                language_limitations.push("compiler inventory identity is unavailable".to_owned());
            }
            let state = if complete { "verified" } else { "partial" };
            limitations.extend(projection_limitations);
            provenance.push(ContextSource {
                authority: "mncs-language".to_owned(),
                path: response.source_path.clone(),
                identity: Some(response.content_identity.clone()),
                state: state.to_owned(),
            });
            LanguageContext {
                source_path: Some(response.source_path),
                current_profile: Some(response.current_profile),
                content_identity: Some(response.content_identity),
                compiler_inventory_identity: response.compiler_inventory_identity,
                mode: response.projection.mode,
                state: state.to_owned(),
                topics,
                modules,
                delta,
                complete,
                limitations: language_limitations,
            }
        }
        Err(error) => {
            let detail = format!("language capability projection unavailable: {error}");
            limitations.push(detail.clone());
            LanguageContext {
                source_path: None,
                current_profile: None,
                content_identity: None,
                compiler_inventory_identity: None,
                mode: "unknown".to_owned(),
                state: "unavailable".to_owned(),
                topics: Vec::new(),
                modules: Vec::new(),
                delta: None,
                complete: false,
                limitations: vec![detail],
            }
        }
    };

    let commons_root = discover_commons_root(workspace.as_deref());
    let commons = match (commons_root.as_deref(), repository_id.as_deref()) {
        (Some(root), Some(repository_id)) => {
            load_commons_projection(root, repository_id, known_architecture_identity, max_items)
        }
        (None, _) => Err("Commons checkout was not found".to_owned()),
        (_, None) => Err("repository identity is unavailable for Commons query".to_owned()),
    };
    let (
        architecture,
        pressures,
        architecture_state,
        pressure_state,
        pressure_registry_identity,
        pressure_view_identity,
    ) = match commons {
        Ok(projection) => {
            limitations.extend(
                projection
                    .limitations
                    .iter()
                    .filter(|item| is_blocking_projection_limitation(item))
                    .cloned(),
            );
            provenance.extend(projection.sources.clone());
            (
                projection.architecture,
                projection.pressures,
                projection.architecture_state,
                projection.pressure_state,
                projection.pressure_registry_identity,
                projection.pressure_view_identity,
            )
        }
        Err(error) => {
            let detail = format!("Commons bounded family projection unavailable: {error}");
            limitations.push(detail.clone());
            (
                unknown_architecture(detail.clone()),
                Vec::new(),
                "unavailable".to_owned(),
                "unavailable".to_owned(),
                None,
                None,
            )
        }
    };

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
    negative_knowledge.extend(negative_knowledge_from_manifest(local_repository.as_ref(), max_items));
    negative_knowledge.sort_by(|left, right| left.identity.cmp(&right.identity));
    negative_knowledge.dedup_by(|left, right| left.identity == right.identity);
    let complete = local_repository
        .as_ref()
        .is_some_and(|item| item.manifest_conformance_state == "verified")
        && language.complete
        && architecture.complete
        && manifest_state == "verified"
        && architecture_state == "current"
        && pressure_state == "current"
        && limitations.is_empty();
    let bounded = language.state != "unavailable"
        && architecture_state != "unavailable"
        && manifest_state != "unavailable";
    let state = if complete {
        "complete"
    } else if bounded {
        "partial"
    } else {
        "unknown"
    };
    let authorities = vec![
        ContextAuthorityState {
            authority: "machine-native-complexity-standard".to_owned(),
            state: manifest_state,
            identity: local_repository
                .as_ref()
                .and_then(|item| item.manifest_validation_identity.clone()),
            registry_identity: None,
        },
        ContextAuthorityState {
            authority: "mncs-language".to_owned(),
            state: language.state.clone(),
            identity: language.content_identity.clone(),
            registry_identity: None,
        },
        ContextAuthorityState {
            authority: "MNCS-Commons.architecture".to_owned(),
            state: architecture_state,
            identity: architecture.content_identity.clone(),
            registry_identity: None,
        },
        ContextAuthorityState {
            authority: "MNCS-Commons.pressures".to_owned(),
            state: pressure_state,
            identity: pressure_view_identity,
            registry_identity: pressure_registry_identity,
        },
        ContextAuthorityState {
            authority: "repository.verification-obligations".to_owned(),
            state: verification.state.clone(),
            identity: verification.inventory_identity.clone(),
            registry_identity: None,
        },
    ];

    Ok(FamilyAgentContextResponse {
        schema_version: FAMILY_CONTEXT_SCHEMA.to_owned(),
        status: ResponseStatus::Answered,
        repository: local_repository,
        language,
        architecture,
        verification,
        atlas,
        pressures,
        negative_knowledge,
        completeness: ContextCompleteness {
            complete,
            state: state.to_owned(),
            limitations,
            authorities,
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
    String,
) {
    let Some(root) = workspace else {
        return (
            None,
            vec!["workspace root is unavailable".to_owned()],
            None,
            "unavailable".to_owned(),
        );
    };
    let path = root.join(".mncs/project.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return (None, Vec::new(), None, "unavailable".to_owned()),
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
                    state: "invalid".to_owned(),
                }),
                "invalid".to_owned(),
            )
        }
    };
    let repository = value
        .get("repository")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let revision = value.get("revision").and_then(Value::as_u64);
    let authority = value.get("authority").cloned();
    let organization = value.get("organization").cloned();
    let contracts = value.get("contracts").cloned();
    let verification = value.get("verification").cloned();
    let validation = load_manifest_validation(root, &path, &identity, repository.as_deref());
    let (conformance_state, validation_identity, mut limitations) = match validation {
        Ok(report)
            if report.get("valid").and_then(Value::as_bool) == Some(true)
                && report
                    .get("validation_identity")
                    .and_then(Value::as_str)
                    .is_some_and(is_content_identity) =>
        {
            (
                "verified".to_owned(),
                report
                    .get("validation_identity")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                Vec::new(),
            )
        }
        Ok(report) if report.get("valid").and_then(Value::as_bool) == Some(true) => (
            "invalid".to_owned(),
            report
                .get("validation_identity")
                .and_then(Value::as_str)
                .map(str::to_owned),
            vec!["Standard validation projection omitted a content identity".to_owned()],
        ),
        Ok(report) => (
            "invalid".to_owned(),
            report
                .get("validation_identity")
                .and_then(Value::as_str)
                .map(str::to_owned),
            vec!["Standard repository-manifest validation failed".to_owned()],
        ),
        Err(error) => (
            "unavailable".to_owned(),
            None,
            vec![format!(
                "Standard repository-manifest validation unavailable: {error}"
            )],
        ),
    };
    if repository.is_none() || revision.is_none() || contracts.is_none() {
        limitations
            .push("local manifest did not expose repository, revision, and contracts".to_owned());
    }
    let source_state = conformance_state.clone();
    (
        Some(RepositoryContext {
            repository,
            revision,
            manifest_path: path.to_string_lossy().into_owned(),
            manifest_identity: identity.clone(),
            manifest_conformance_state: conformance_state.clone(),
            manifest_validation_identity: validation_identity,
            authority,
            organization,
            contracts,
            verification,
            present: true,
        }),
        limitations,
        Some(ContextSource {
            authority: "repository".to_owned(),
            path: path.to_string_lossy().into_owned(),
            identity: Some(identity),
            state: source_state,
        }),
        conformance_state,
    )
}

fn load_verification_context(
    workspace: Option<&Path>,
    repository: Option<&RepositoryContext>,
    max_items: usize,
) -> (VerificationContext, Vec<NegativeKnowledge>, Option<ContextSource>) {
    let unavailable = |state: &str, limitation: String| {
        (
            VerificationContext {
                state: state.to_owned(),
                inventory_path: None,
                inventory_identity: None,
                repository: repository.and_then(|item| item.repository.clone()),
                revision: None,
                obligations: Vec::new(),
                complete: false,
                limitations: vec![limitation],
            },
            Vec::new(),
            None,
        )
    };
    let Some(root) = workspace else {
        return unavailable("unavailable", "workspace root is unavailable".to_owned());
    };
    let Some(repository) = repository else {
        return unavailable(
            "unavailable",
            "repository manifest is unavailable; verification obligation ownership is UNKNOWN"
                .to_owned(),
        );
    };
    let Some(declaration) = repository.verification.as_ref() else {
        return (
            VerificationContext {
                state: "not_declared".to_owned(),
                inventory_path: None,
                inventory_identity: None,
                repository: repository.repository.clone(),
                revision: None,
                obligations: Vec::new(),
                complete: false,
                limitations: vec![
                    "repository does not declare a verification obligation inventory".to_owned(),
                ],
            },
            Vec::new(),
            None,
        );
    };
    if declaration.get("schema_version").and_then(Value::as_str)
        != Some(OBLIGATION_INVENTORY_SCHEMA)
    {
        return unavailable(
            "invalid",
            format!(
                "verification declaration must use {OBLIGATION_INVENTORY_SCHEMA}"
            ),
        );
    }
    let Some(relative) = declaration
        .get("obligation_inventory")
        .and_then(Value::as_str)
    else {
        return unavailable(
            "invalid",
            "verification declaration does not name obligation_inventory".to_owned(),
        );
    };
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() || relative_path.components().any(|component| {
        matches!(component, std::path::Component::ParentDir)
    }) {
        return unavailable(
            "invalid",
            "verification obligation inventory path is not a safe repository-relative path"
                .to_owned(),
        );
    }
    let path = root.join(relative_path);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return unavailable(
                "unavailable",
                format!("verification obligation inventory cannot be read: {error}"),
            )
        }
    };
    let identity = sha256(&bytes);
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return (
                VerificationContext {
                    state: "invalid".to_owned(),
                    inventory_path: Some(path.to_string_lossy().into_owned()),
                    inventory_identity: Some(identity.clone()),
                    repository: repository.repository.clone(),
                    revision: None,
                    obligations: Vec::new(),
                    complete: false,
                    limitations: vec![format!(
                        "verification obligation inventory is invalid JSON: {error}"
                    )],
                },
                Vec::new(),
                Some(ContextSource {
                    authority: "repository.verification-obligations".to_owned(),
                    path: path.to_string_lossy().into_owned(),
                    identity: Some(identity),
                    state: "invalid".to_owned(),
                }),
            )
        }
    };
    if value.get("schema_version").and_then(Value::as_str) != Some(OBLIGATION_INVENTORY_SCHEMA) {
        return (
            VerificationContext {
                state: "invalid".to_owned(),
                inventory_path: Some(path.to_string_lossy().into_owned()),
                inventory_identity: Some(identity.clone()),
                repository: repository.repository.clone(),
                revision: None,
                obligations: Vec::new(),
                complete: false,
                limitations: vec![format!(
                    "verification obligation inventory must be {OBLIGATION_INVENTORY_SCHEMA}"
                )],
            },
            Vec::new(),
            Some(ContextSource {
                authority: "repository.verification-obligations".to_owned(),
                path: path.to_string_lossy().into_owned(),
                identity: Some(identity),
                state: "invalid".to_owned(),
            }),
        );
    }
    let inventory_repository = value.get("repository").and_then(Value::as_str);
    let revision = value.get("revision").and_then(Value::as_u64);
    let raw_obligations = value.get("obligations").and_then(Value::as_array);
    if inventory_repository != repository.repository.as_deref()
        || revision.is_none()
        || raw_obligations.is_none()
    {
        return (
            VerificationContext {
                state: "invalid".to_owned(),
                inventory_path: Some(path.to_string_lossy().into_owned()),
                inventory_identity: Some(identity.clone()),
                repository: repository.repository.clone(),
                revision,
                obligations: Vec::new(),
                complete: false,
                limitations: vec![
                    "verification obligation inventory is not bound to the local repository"
                        .to_owned(),
                ],
            },
            Vec::new(),
            Some(ContextSource {
                authority: "repository.verification-obligations".to_owned(),
                path: path.to_string_lossy().into_owned(),
                identity: Some(identity),
                state: "invalid".to_owned(),
            }),
        );
    }
    let raw_obligations = raw_obligations.expect("checked above");
    let truncated = raw_obligations.len() > max_items;
    let mut obligations = Vec::new();
    let mut negative = Vec::new();
    for item in raw_obligations.iter().take(max_items) {
        let Some(identity_value) = item.get("identity").and_then(Value::as_str) else {
            return unavailable(
                "invalid",
                "verification obligation inventory contains an obligation without identity"
                    .to_owned(),
            );
        };
        let lifecycle = item
            .get("lifecycle")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(identity_value);
        let domain = item
            .get("guarantee_domain")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = item
            .get("evidence_role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let scope = item
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let executor = item.get("executor").and_then(Value::as_object);
        let provider = executor
            .and_then(|value| value.get("provider"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let kind = executor
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let ordinary = matches!(lifecycle, "permanent" | "transitional");
        obligations.push(VerificationObligationSummary {
            identity: identity_value.to_owned(),
            title: title.to_owned(),
            guarantee_domain: domain.to_owned(),
            evidence_role: role.to_owned(),
            lifecycle: lifecycle.to_owned(),
            scope: scope.to_owned(),
            executor_provider: provider.to_owned(),
            executor_kind: kind.to_owned(),
            ordinary_verification: ordinary,
        });
        if !ordinary {
            let disposition = match lifecycle {
                "reference_only" => "historical_reference",
                "scheduled" => "scheduled_only",
                "retired" => "retired",
                _ => "not_ordinary_verification",
            };
            negative.push(NegativeKnowledge {
                identity: identity_value.to_owned(),
                disposition: disposition.to_owned(),
                source: path.to_string_lossy().into_owned(),
                reason: format!(
                    "obligation lifecycle {lifecycle} is not part of ordinary verification"
                ),
            });
        }
    }
    let mut limitations = Vec::new();
    if truncated {
        limitations.push("verification obligation rows are bounded by the family context limit".to_owned());
    }
    let state = if truncated { "truncated" } else { "current" };
    (
        VerificationContext {
            state: state.to_owned(),
            inventory_path: Some(path.to_string_lossy().into_owned()),
            inventory_identity: Some(identity.clone()),
            repository: repository.repository.clone(),
            revision,
            obligations,
            complete: !truncated,
            limitations,
        },
        negative,
        Some(ContextSource {
            authority: "repository.verification-obligations".to_owned(),
            path: path.to_string_lossy().into_owned(),
            identity: Some(identity),
            state: state.to_owned(),
        }),
    )
}

fn negative_knowledge_from_manifest(
    repository: Option<&RepositoryContext>,
    max_items: usize,
) -> Vec<NegativeKnowledge> {
    let Some(organization) = repository.and_then(|item| item.organization.as_ref()) else {
        return Vec::new();
    };
    let Some(surfaces) = organization.get("surfaces").and_then(Value::as_array) else {
        return Vec::new();
    };
    surfaces
        .iter()
        .filter_map(|surface| {
            let path = surface.get("path").and_then(Value::as_str)?;
            let class = surface.get("class").and_then(Value::as_str).unwrap_or("");
            let classification = surface.get("classification").and_then(Value::as_object);
            let lifecycle = classification
                .and_then(|value| value.get("lifecycle"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_migration = class == "migration-shadow" || lifecycle == "temporary";
            let is_retired = class == "retired" || lifecycle == "retired" || lifecycle == "historical";
            if !is_migration && !is_retired {
                return None;
            }
            let (disposition, reason) = if is_retired {
                (
                    "retired",
                    "repository surface is historical or retired and is not a canonical implementation path",
                )
            } else {
                (
                    "migration_only",
                    "repository surface is temporary migration machinery and is not canonical authority",
                )
            };
            Some(NegativeKnowledge {
                identity: format!("repository-surface:{path}"),
                disposition: disposition.to_owned(),
                source: repository
                    .map(|item| item.manifest_path.clone())
                    .unwrap_or_else(|| "manifest".to_owned()),
                reason: reason.to_owned(),
            })
        })
        .take(max_items)
        .collect()
}

fn load_manifest_validation(
    repository_root: &Path,
    manifest_path: &Path,
    manifest_identity: &str,
    repository: Option<&str>,
) -> Result<Value, String> {
    if let Some(path) = env::var_os("MNCS_STANDARD_VALIDATION_PATH") {
        let value: Value = serde_json::from_slice(
            &fs::read(path)
                .map_err(|error| format!("validation projection read failed: {error}"))?,
        )
        .map_err(|error| format!("validation projection JSON is invalid: {error}"))?;
        if value.get("schema_version").and_then(Value::as_str) != Some(MANIFEST_VALIDATION_SCHEMA)
            || value.get("manifest_identity").and_then(Value::as_str) != Some(manifest_identity)
        {
            return Err("validation projection is not bound to this manifest".to_owned());
        }
        return Ok(value);
    }
    let standard_root = discover_standard_root(repository_root)
        .ok_or_else(|| "Standard checkout was not found".to_owned())?;
    let command = env::var_os("MNCS_STANDARD_COMMAND").unwrap_or_else(|| "python3".into());
    let manifest = manifest_path.to_string_lossy().into_owned();
    let root = repository_root.to_string_lossy().into_owned();
    let repository = repository.unwrap_or_default().to_owned();
    let args = [
        "scripts/validate-family-manifest.py",
        "--manifest",
        manifest.as_str(),
        "--repository-root",
        root.as_str(),
        "--repository",
        repository.as_str(),
    ];
    let value = run_json_command(&command, &args, &standard_root, None, true)?;
    if value.get("schema_version").and_then(Value::as_str) != Some(MANIFEST_VALIDATION_SCHEMA)
        || value.get("manifest_identity").and_then(Value::as_str) != Some(manifest_identity)
    {
        return Err("Standard validation projection is not bound to this manifest".to_owned());
    }
    Ok(value)
}

fn load_commons_projection(
    root: &Path,
    repository: &str,
    known_architecture_identity: Option<&str>,
    max_items: usize,
) -> Result<CommonsProjection, String> {
    let value = if let Some(path) = env::var_os("MNCS_COMMONS_PROJECTION_PATH") {
        serde_json::from_slice(
            &fs::read(path).map_err(|error| format!("Commons projection read failed: {error}"))?,
        )
        .map_err(|error| format!("Commons projection JSON is invalid: {error}"))?
    } else {
        let command = env::var_os("MNCS_COMMONS_COMMAND").unwrap_or_else(|| "python3".into());
        let mut args = vec![
            "-m".to_owned(),
            "mncs_commons.cli".to_owned(),
            "family".to_owned(),
            "agent-context".to_owned(),
            "--root".to_owned(),
            root.to_string_lossy().into_owned(),
            "--repository".to_owned(),
            repository.to_owned(),
            "--max-items".to_owned(),
            max_items.to_string(),
        ];
        if let Some(identity) = known_architecture_identity {
            args.push("--since-architecture".to_owned());
            args.push(identity.to_owned());
        }
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let pythonpath = root.join("src");
        run_json_command(&command, &refs, root, Some(&pythonpath), false)?
    };
    parse_commons_projection(value, root, max_items)
}

fn parse_commons_projection(
    value: Value,
    root: &Path,
    max_items: usize,
) -> Result<CommonsProjection, String> {
    if value.get("schema_version").and_then(Value::as_str) != Some(COMMONS_PROJECTION_SCHEMA) {
        return Err("Commons projection schema is unsupported".to_owned());
    }
    let projection_identity = value
        .get("projection_identity")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let projection_identity_valid = projection_identity
        .as_deref()
        .is_some_and(is_content_identity);
    let freshness = value
        .get("freshness")
        .and_then(Value::as_str)
        .unwrap_or("invalid");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("invalid");
    let root_limitations = value
        .get("limitations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let architecture_value = value.get("architecture").cloned().unwrap_or(Value::Null);
    let architecture_query = architecture_value
        .get("query")
        .cloned()
        .unwrap_or(Value::Null);
    let projection = architecture_query
        .get("projection")
        .cloned()
        .unwrap_or(Value::Null);
    let mut architecture_limitations = root_limitations.clone();
    architecture_limitations.extend(
        projection
            .get("limitations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    let capabilities = architecture_query
        .get("scoped_capabilities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_items)
        .collect::<Vec<_>>();
    let generators = architecture_query
        .get("generators")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_items)
        .collect::<Vec<_>>();
    if architecture_query
        .get("scoped_capabilities")
        .and_then(Value::as_array)
        .is_some_and(|items| items.len() > max_items)
    {
        architecture_limitations.push("architecture projection truncated at max_items".to_owned());
    }
    let architecture_freshness = architecture_value
        .get("freshness")
        .and_then(Value::as_str)
        .unwrap_or(freshness);
    let architecture_state = if status != "verified"
        || !projection_identity_valid
        || architecture_value
            .get("validation_state")
            .and_then(Value::as_str)
            != Some("verified")
    {
        "invalid"
    } else if architecture_freshness == "current" {
        "current"
    } else if architecture_freshness == "truncated" {
        "truncated"
    } else if architecture_freshness == "stale" {
        "stale"
    } else {
        "unavailable"
    };
    let architecture_complete = architecture_state == "current"
        && architecture_value
            .get("schema_identity")
            .and_then(Value::as_str)
            .is_some()
        && architecture_value
            .get("content_identity")
            .and_then(Value::as_str)
            .is_some()
        && architecture_value
            .get("validation_identity")
            .and_then(Value::as_str)
            .is_some()
        && projection.get("complete").and_then(Value::as_bool) == Some(true)
        && !architecture_limitations
            .iter()
            .any(|item| is_blocking_projection_limitation(item));
    let architecture = ArchitectureContext {
        source_path: Some(format!("{}:family-agent-context", root.display())),
        schema_identity: architecture_value
            .get("schema_identity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        content_identity: architecture_value
            .get("content_identity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        validation_identity: architecture_value
            .get("validation_identity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        validation_state: architecture_value
            .get("validation_state")
            .and_then(Value::as_str)
            .unwrap_or("invalid")
            .to_owned(),
        freshness: architecture_freshness.to_owned(),
        projection_identity: projection_identity.clone(),
        mode: projection
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        capabilities,
        generators,
        delta: architecture_query.get("delta").cloned(),
        complete: architecture_complete,
        limitations: architecture_limitations,
    };

    let pressure_value = value.get("pressures").cloned().unwrap_or(Value::Null);
    let pressure_freshness = pressure_value
        .get("freshness")
        .and_then(Value::as_str)
        .unwrap_or(freshness);
    let pressure_identities_present = pressure_value
        .get("registry_identity")
        .and_then(Value::as_str)
        .is_some()
        && pressure_value
            .get("view_identity")
            .and_then(Value::as_str)
            .is_some();
    let pressure_rows = pressure_value
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pressure_rows_truncated = pressure_rows.len() > max_items;
    let pressure_state = if status != "verified"
        || !projection_identity_valid
        || pressure_value
            .get("validation_state")
            .and_then(Value::as_str)
            != Some("verified")
        || !pressure_identities_present
    {
        "invalid"
    } else if pressure_rows_truncated {
        "truncated"
    } else if pressure_freshness == "current" {
        "current"
    } else if pressure_freshness == "truncated" {
        "truncated"
    } else if pressure_freshness == "stale" {
        "stale"
    } else if pressure_freshness == "unavailable" {
        "unavailable"
    } else {
        "invalid"
    };
    let mut pressures = Vec::new();
    for item in pressure_rows.into_iter().take(max_items) {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            pressures.push(PressureSummary {
                id: id.to_owned(),
                title: item.get("title").and_then(Value::as_str).map(str::to_owned),
                target: item
                    .get("target")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                domain: item
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                severity: item
                    .get("severity")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                status: item
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                verification_state: item
                    .get("verificationState")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                unresolved: item.get("unresolved").and_then(Value::as_bool),
                affected_repositories: item
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
                required_behavior: item
                    .get("requiredBehavior")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                source_path: format!("{}:family-agent-context", root.display()),
            });
        }
    }
    let mut limitations = root_limitations;
    if pressure_state != "current" {
        limitations.push("Commons pressure projection is not current and verified".to_owned());
    }
    if architecture_state != "current" {
        limitations.push("Commons architecture projection is not current and verified".to_owned());
    }
    Ok(CommonsProjection {
        architecture,
        pressures,
        limitations,
        sources: vec![
            ContextSource {
                authority: "MNCS-Commons.architecture".to_owned(),
                path: format!("{}:family-agent-context", root.display()),
                identity: architecture_value
                    .get("content_identity")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                state: architecture_state.to_owned(),
            },
            ContextSource {
                authority: "MNCS-Commons.pressures".to_owned(),
                path: format!("{}:family-agent-context", root.display()),
                identity: pressure_value
                    .get("view_identity")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                state: pressure_state.to_owned(),
            },
        ],
        architecture_state: architecture_state.to_owned(),
        pressure_state: pressure_state.to_owned(),
        pressure_registry_identity: pressure_value
            .get("registry_identity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        pressure_view_identity: pressure_value
            .get("view_identity")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn is_blocking_projection_limitation(value: &str) -> bool {
    [
        "truncated",
        "stale",
        "unavailable",
        "validation failed",
        "not current",
        "invalid",
    ]
    .iter()
    .any(|marker| value.to_ascii_lowercase().contains(marker))
}

fn is_content_identity(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn language_delta_has_more(value: &LanguageDelta, max_items: usize) -> bool {
    value.profiles.len() > max_items
        || change_set_has_more(&value.profile_changes, max_items)
        || change_set_has_more(&value.modules, max_items)
        || change_set_has_more(&value.exports, max_items)
        || change_set_has_more(&value.intrinsics, max_items)
        || change_set_has_more(&value.effects, max_items)
        || change_set_has_more(&value.capabilities, max_items)
        || change_set_has_more(&value.canonical_examples, max_items)
}

fn change_set_has_more(value: &LanguageChangeSet, max_items: usize) -> bool {
    value.added.len() > max_items
        || value.changed.len() > max_items
        || value.removed.len() > max_items
}

fn bound_language_delta(value: &mut LanguageDelta, max_items: usize) {
    value.profiles.truncate(max_items);
    for changes in [
        &mut value.profile_changes,
        &mut value.modules,
        &mut value.exports,
        &mut value.intrinsics,
        &mut value.effects,
        &mut value.capabilities,
        &mut value.canonical_examples,
    ] {
        changes.added.truncate(max_items);
        changes.changed.truncate(max_items);
        changes.removed.truncate(max_items);
    }
}

fn run_json_command(
    command: &std::ffi::OsStr,
    args: &[&str],
    current_dir: &Path,
    pythonpath: Option<&Path>,
    allow_nonzero_json: bool,
) -> Result<Value, String> {
    let mut process = Command::new(command);
    process
        .args(args)
        .current_dir(current_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = pythonpath {
        let existing = env::var_os("PYTHONPATH").unwrap_or_default();
        let mut joined = path.as_os_str().to_os_string();
        if !existing.is_empty() {
            joined.push(":");
            joined.push(existing);
        }
        process.env("PYTHONPATH", joined);
    }
    let mut child = process
        .spawn()
        .map_err(|error| format!("authority query could not start: {error}"))?;
    let deadline = Instant::now() + AUTHORITY_QUERY_TIMEOUT;
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("authority query wait failed: {error}"))?
        {
            Some(status) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("authority query output failed: {error}"))?;
                if output.stdout.len() > MAX_AUTHORITY_OUTPUT {
                    return Err("authority query output exceeded its bound".to_owned());
                }
                if !status.success() && !allow_nonzero_json {
                    return Err(format!(
                        "authority query exited unsuccessfully: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                return serde_json::from_slice(&output.stdout)
                    .map_err(|error| format!("authority query returned invalid JSON: {error}"));
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                return Err("authority query exceeded its time bound".to_owned());
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn discover_standard_root(workspace: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("MNCS_STANDARD_ROOT") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(workspace.to_path_buf());
    let mut ancestor = workspace.parent();
    for _ in 0..5 {
        let Some(path) = ancestor else { break };
        candidates.push(path.join("machine-native-complexity-standard"));
        ancestor = path.parent();
    }
    candidates
        .into_iter()
        .find(|path| path.join("scripts/validate-family-manifest.py").is_file())
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
        for _ in 0..5 {
            let Some(path) = ancestor else { break };
            candidates.push(path.join("MNCS-Commons"));
            ancestor = path.parent();
        }
    }
    candidates.into_iter().find(|candidate| {
        candidate
            .join("family/architecture-model-v1.json")
            .is_file()
            && candidate
                .join("src/mncs_commons/family_projection.py")
                .is_file()
    })
}

fn unknown_architecture(reason: String) -> ArchitectureContext {
    ArchitectureContext {
        source_path: None,
        schema_identity: None,
        content_identity: None,
        validation_identity: None,
        validation_state: "unavailable".to_owned(),
        freshness: "unavailable".to_owned(),
        projection_identity: None,
        mode: "unknown".to_owned(),
        capabilities: Vec::new(),
        generators: Vec::new(),
        delta: None,
        complete: false,
        limitations: vec![reason],
    }
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
        for _ in 0..5 {
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
        registry_revision: value.get("registry_revision").and_then(Value::as_str).map(str::to_owned),
        authority: "orientation-only; owning repositories, Standard, language, and Commons retain authority".to_owned(),
        state: "orientation".to_owned(),
        project,
    };
    (
        Some(context),
        Some(ContextSource {
            authority: "mncs-atlas (non-normative orientation)".to_owned(),
            path: path.to_string_lossy().into_owned(),
            identity: Some(sha256(&bytes)),
            state: "orientation".to_owned(),
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

    #[test]
    fn missing_required_authorities_remain_unknown() {
        let root = env::temp_dir().join(format!(
            "mncs-family-context-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".mncs")).unwrap();
        fs::write(root.join(".mncs/project.json"), r#"{"schema_version":"mncs-family.repository-manifest/v0alpha1","repository":"fixture-repo","revision":1,"contracts":{"provides":[],"consumes":[],"tests":[]}}"#).unwrap();
        let response = query(Some(&root), None, None, None, None, None, 8).unwrap();
        assert_eq!(response.completeness.state, "unknown");
        assert!(!response.completeness.complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_projection_is_not_complete() {
        let value = serde_json::json!({"schema_version": COMMONS_PROJECTION_SCHEMA, "status": "invalid", "freshness": "invalid", "architecture": {"query": {"projection": {"mode": "unknown"}}}, "pressures": {"validation_state": "invalid"}});
        let parsed = parse_commons_projection(value, Path::new("/commons"), 8).unwrap();
        assert!(!parsed.architecture.complete);
        assert_eq!(parsed.architecture_state, "invalid");
        assert_eq!(parsed.pressure_state, "invalid");
    }

    #[test]
    fn stale_pressure_view_is_not_current_or_complete() {
        let value = serde_json::json!({
            "schema_version": COMMONS_PROJECTION_SCHEMA,
            "projection_identity": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "status": "verified",
            "freshness": "stale",
            "architecture": {
                "schema_identity": "commons.mncs.architecture-model/1",
                "content_identity": "sha256:architecture",
                "validation_identity": "sha256:validation",
                "validation_state": "verified",
                "freshness": "current",
                "query": {"projection": {"complete": true, "mode": "targeted"}}
            },
            "pressures": {
                "registry_identity": "sha256:registry",
                "view_identity": "sha256:view",
                "validation_state": "verified",
                "freshness": "stale",
                "rows": []
            }
        });
        let parsed = parse_commons_projection(value, Path::new("/commons"), 8).unwrap();
        assert!(parsed.architecture.complete);
        assert_eq!(parsed.architecture_state, "current");
        assert_eq!(parsed.pressure_state, "stale");
        assert!(!parsed.limitations.is_empty());
    }
}
