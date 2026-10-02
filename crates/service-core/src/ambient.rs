//! Ambient semantic-coherence surface: resident status and bounded capsules.
//!
//! This module is the machine-native observation boundary of the resident
//! service. It deliberately contains no language semantics of its own:
//!
//! - resident status projects process/workspace/toolchain bindings;
//! - the semantic capsule projects measured findings into a fixed envelope;
//! - every admission decision inside the capsule is executed by the frozen
//!   MNCS policy in `mncs/semantic_capsule.mncs` through the authoritative
//!   compiler and research-bytecode backend.
//!
//! The host applies the policy's admission flags mechanically and must not
//! widen them. There is intentionally no Rust control reimplementation of
//! the admission policy; correctness is established by executed-behavior
//! tests against the real kernel, not by a duplicated decision procedure.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use mncs_codegen::{execute_backend, RESEARCH_BYTECODE_BACKEND_NAME};
use mncs_compiler::{ReferenceCompiler, SourceFrontEndResult};
use mncs_model::{
    ArtifactRepresentation, CompilationStatus, ExecutionRequest, ExecutionStatus, ExecutionTarget,
    ExecutionValue, IntegerType, Program, TransformationStatus, EXECUTION_REQUEST_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::document::DocumentStore;
use crate::error::ServiceError;
use crate::modules::StoreResolver;
use crate::queries::{ResponseStatus, SnapshotInfo};

pub const SERVICE_STATUS_SCHEMA_VERSION: &str = "mncs.language-service.service-status/1";
pub const SEMANTIC_CAPSULE_SCHEMA_VERSION: &str = "mncs.language-service.semantic-capsule/1";

/// Fixed policy envelope: at most this many measured findings are projected
/// into one capsule evaluation. The bound is shared with the MNCS module.
pub const CAPSULE_ENVELOPE_CAPACITY: usize = 32;

/// Ambient-relevant feature flags for one service build. These name stable
/// query families, not every RPC method.
pub const AMBIENT_FEATURES: &[&str] = &[
    "event-stream/2",
    "semantic-capsule/1",
    "candidate-analysis",
    "debug-source-binding/1",
    "language-capabilities",
    "dependency-graph",
];

pub(crate) const CAPSULE_MODULE: &str = "mncs.language_service.semantic_capsule.v1";
pub(crate) const CAPSULE_FUNCTION: &str = "select_capsule";
const CAPSULE_URI: &str = "mncs://language-service/semantic-capsule.mncs";
const CAPSULE_SOURCE: &str = include_str!("../../../mncs/semantic_capsule.mncs");
const CAPSULE_STEP_BUDGET: u64 = 100_000;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Process-bound service identity. The instance id is random per process so
/// two hosts never share one, even across a checkpoint restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIdentity {
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Content fingerprint of the running build (version + executable path +
    /// modification time). Rebuilding the host changes this value.
    pub build_fingerprint: String,
}

/// Toolchain binding observed from the process environment. The provider
/// owns measurement (it launches the host with these variables); the
/// service binds and echoes them so Environment can detect drift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_path: Option<String>,
    /// Provider-supplied pin (for example a language checkout revision).
    /// Compared verbatim when present on either side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

impl ToolchainIdentity {
    pub fn current() -> Self {
        Self {
            language_root: std::env::var("MNCS_LANGUAGE_ROOT").ok(),
            library_path: std::env::var("MNCS_LIBRARY_PATH").ok(),
            pinned: std::env::var("MNLS_TOOLCHAIN_IDENTITY").ok(),
        }
    }

    /// Stable digest over the bound fields for compact comparison.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"mncs.language-service.toolchain/1\n");
        for field in [
            self.language_root.as_deref().unwrap_or(""),
            self.library_path.as_deref().unwrap_or(""),
            self.pinned.as_deref().unwrap_or(""),
        ] {
            hasher.update(field.as_bytes());
            hasher.update([0]);
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

pub(crate) fn new_instance_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("mnls-{}-{nanos}", std::process::id())
}

pub(crate) fn build_fingerprint() -> (Option<String>, String) {
    let executable = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    let mtime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"mnls-build/1\n");
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update([0]);
    hasher.update(executable.as_deref().unwrap_or("").as_bytes());
    hasher.update([0]);
    hasher.update(mtime.as_bytes());
    (executable, format!("sha256:{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Resident status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticTotals {
    pub error: usize,
    pub warning: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationTotals {
    pub pass: usize,
    pub fail: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessState {
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointObservation {
    pub stream_identity: String,
    pub last_generation: u64,
    pub last_cursor: u64,
    pub toolchain_matches_current: bool,
}

/// Resident status: everything an ambient consumer needs to decide whether
/// its semantic view is current, without triggering recomputation beyond
/// the resident analysis cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatusResponse {
    pub schema_version: String,
    pub service: ServiceIdentity,
    pub workspace_root: Option<String>,
    pub toolchain: ToolchainIdentity,
    pub toolchain_digest: String,
    pub generation: u64,
    pub stream_identity: String,
    pub event_cursor: u64,
    pub documents: usize,
    pub diagnostics: DiagnosticTotals,
    pub obligations: ObligationTotals,
    pub readiness: ReadinessState,
    pub features: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

// ---------------------------------------------------------------------------
// Semantic capsule
// ---------------------------------------------------------------------------

/// One measured finding projected into the policy envelope. Severity and
/// kind ranks are shared with the MNCS module: severity 0 = info,
/// 1 = warning, 2 = error; kind 0 = diagnostic, 1 = changed subject,
/// 2 = obligation, 3 = affected module.
#[derive(Debug, Clone)]
pub(crate) struct MeasuredFinding {
    pub severity_rank: i64,
    pub kind_rank: i64,
    pub key: String,
    pub summary: String,
    pub uri: Option<String>,
    pub identity: Option<String>,
    pub code: Option<String>,
}

/// Exact follow-up invocation for an admitted finding. The method names a
/// resident RPC; params are its exact arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionHandle {
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleFinding {
    pub kind: String,
    pub relevance: String,
    pub severity: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub expansion: ExpansionHandle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeasuredTotals {
    pub diagnostics: usize,
    pub changed_subjects: usize,
    pub obligations: usize,
    pub affected_modules: usize,
    /// Candidates dropped before policy evaluation because the fixed
    /// envelope was full. Deterministic priority order keeps this stable.
    pub truncated_by_envelope: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsulePolicyEvidence {
    pub backend: String,
    pub kernel_source_identity: String,
    pub kernel_artifact_identity: String,
    pub admitted: usize,
    pub actionable: usize,
    pub watch: usize,
    pub dropped: usize,
    pub complete: bool,
    pub valid: bool,
}

/// Bounded semantic capsule: the minimum current semantic state an agent
/// needs to start productive work. Deep state stays resident and is
/// reachable through each finding's expansion handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCapsuleResponse {
    pub schema_version: String,
    pub status: ResponseStatus,
    pub workspace_root: Option<String>,
    pub generation: u64,
    pub stream_identity: String,
    pub after_cursor: u64,
    pub current_cursor: u64,
    /// False when the caller's cursor named a different stream: the window
    /// was reconciled to current state instead of resumed.
    pub window_matched: bool,
    pub measured: MeasuredTotals,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<CapsulePolicyEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<CapsuleFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
}

// ---------------------------------------------------------------------------
// MNCS capsule kernel
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct CapsuleKernel {
    source_identity: String,
    program: Arc<Program>,
    artifact: Arc<mncs_model::BackendArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapsuleSelection {
    pub admitted: Vec<bool>,
    pub relevance: Vec<i64>,
    pub admitted_count: usize,
    pub actionable_count: usize,
    pub watch_count: usize,
    pub dropped_count: usize,
    pub complete: bool,
    pub valid: bool,
    pub kernel_source_identity: String,
    pub kernel_artifact_identity: String,
    pub backend: String,
}

fn frontend_error(front_end: &SourceFrontEndResult) -> String {
    let details = front_end
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>();
    if details.is_empty() {
        "capsule policy kernel did not produce a valid authoritative program".to_owned()
    } else {
        format!(
            "capsule policy kernel source is unsupported: {}",
            details.join("; ")
        )
    }
}

fn prepare_kernel(store: &DocumentStore, source_identity: &str) -> Result<CapsuleKernel, String> {
    let resolver = StoreResolver::new(store);
    let envelope = store.envelope(CAPSULE_URI, CAPSULE_SOURCE);
    let compiler = ReferenceCompiler::default();
    let front_end = compiler.front_end_with_resolver(envelope, &resolver);
    if !front_end.is_valid() {
        return Err(frontend_error(&front_end));
    }
    let program = Arc::new(
        front_end
            .program
            .clone()
            .ok_or_else(|| frontend_error(&front_end))?,
    );
    let request = compiler
        .request_for_program_with_backend(
            &program,
            std::collections::BTreeSet::from([ArtifactRepresentation::BackendArtifact]),
            RESEARCH_BYTECODE_BACKEND_NAME,
        )
        .map_err(|diagnostic| {
            format!(
                "capsule policy kernel request refused: {}",
                diagnostic.message
            )
        })?;
    let compilation = compiler.compile(request, &program);
    if compilation.status == CompilationStatus::Failed {
        let details = compilation
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
            .collect::<Vec<_>>();
        return Err(format!(
            "capsule policy kernel compilation failed: {}",
            details.join("; ")
        ));
    }
    let artifact = compilation
        .emissions
        .backend
        .ok_or_else(|| "capsule policy kernel backend artifact was not emitted".to_owned())?;
    if artifact.backend.name != RESEARCH_BYTECODE_BACKEND_NAME
        || !artifact.identity_is_valid()
        || artifact.status != TransformationStatus::Pass
    {
        return Err(
            "capsule policy kernel backend artifact failed identity/status validation".to_owned(),
        );
    }
    Ok(CapsuleKernel {
        source_identity: source_identity.to_owned(),
        program,
        artifact: Arc::new(artifact),
    })
}

fn kernel(
    cache: &RwLock<Option<Arc<CapsuleKernel>>>,
    store: &DocumentStore,
) -> Result<Arc<CapsuleKernel>, String> {
    let envelope = store.envelope(CAPSULE_URI, CAPSULE_SOURCE);
    if let Ok(read) = cache.read() {
        if let Some(existing) = read.as_ref() {
            if existing.source_identity == envelope.identity {
                return Ok(Arc::clone(existing));
            }
        }
    }
    let prepared = Arc::new(prepare_kernel(store, &envelope.identity)?);
    let mut write = cache
        .write()
        .map_err(|_| "capsule policy kernel cache is poisoned".to_owned())?;
    if let Some(existing) = write.as_ref() {
        if existing.source_identity == prepared.source_identity {
            return Ok(Arc::clone(existing));
        }
    }
    *write = Some(Arc::clone(&prepared));
    Ok(prepared)
}

fn integer_argument(value: i64) -> ExecutionValue {
    ExecutionValue::Integer {
        value: value as i128,
        ty: IntegerType {
            bits: 64,
            signed: true,
        },
    }
}

fn record_argument(
    program: &Program,
    name: &str,
    fields: Vec<(String, ExecutionValue)>,
) -> Result<ExecutionValue, String> {
    let record = program
        .record_types
        .iter()
        .find(|record| record.name == name)
        .ok_or_else(|| format!("capsule policy kernel does not expose record {name}"))?;
    Ok(ExecutionValue::Record {
        type_identity: record.identity.clone(),
        name: record.name.clone(),
        fields: Arc::new(fields),
    })
}

fn integer_field(value: &ExecutionValue, field: &str) -> Result<i64, String> {
    let ExecutionValue::Integer { value, ty } = value else {
        return Err(format!(
            "capsule selection field {field:?} is not an integer"
        ));
    };
    if *ty
        != (IntegerType {
            bits: 64,
            signed: true,
        })
        || *value < 0
    {
        return Err(format!(
            "capsule selection field {field:?} has an invalid integer value"
        ));
    }
    i64::try_from(*value).map_err(|_| format!("capsule selection field {field:?} is too large"))
}

fn bool_field(value: &ExecutionValue, field: &str) -> Result<bool, String> {
    let ExecutionValue::Boolean { value } = value else {
        return Err(format!("capsule selection field {field:?} is not boolean"));
    };
    Ok(*value)
}

fn bool_sequence_field(value: &ExecutionValue, field: &str) -> Result<Vec<bool>, String> {
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!(
            "capsule selection field {field:?} is not a sequence"
        ));
    };
    if values.len() != CAPSULE_ENVELOPE_CAPACITY {
        return Err(format!(
            "capsule selection field {field:?} has length {} instead of {CAPSULE_ENVELOPE_CAPACITY}",
            values.len()
        ));
    }
    values
        .iter()
        .map(|value| match value {
            ExecutionValue::Boolean { value } => Ok(*value),
            _ => Err(format!(
                "capsule selection field {field:?} is not a boolean sequence"
            )),
        })
        .collect()
}

fn integer_sequence_field(value: &ExecutionValue, field: &str) -> Result<Vec<i64>, String> {
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!(
            "capsule selection field {field:?} is not a sequence"
        ));
    };
    if values.len() != CAPSULE_ENVELOPE_CAPACITY {
        return Err(format!(
            "capsule selection field {field:?} has length {} instead of {CAPSULE_ENVELOPE_CAPACITY}",
            values.len()
        ));
    }
    values
        .iter()
        .map(|value| integer_field(value, field))
        .collect()
}

/// Execute the frozen capsule admission policy over one measured envelope.
///
/// `findings` are `(severity_rank, kind_rank)` pairs in deterministic
/// priority order; unused slots are padded with rank-zero findings and
/// excluded by `count`. Budgets live inside the MNCS module; the host
/// supplies only measured ranks and the active count.
pub(crate) fn execute_capsule_selection(
    cache: &RwLock<Option<Arc<CapsuleKernel>>>,
    store: &DocumentStore,
    findings: &[(i64, i64)],
    count: usize,
) -> Result<CapsuleSelection, String> {
    if findings.len() > CAPSULE_ENVELOPE_CAPACITY || count > CAPSULE_ENVELOPE_CAPACITY {
        return Err(format!(
            "capsule policy envelope is bounded to {CAPSULE_ENVELOPE_CAPACITY} findings"
        ));
    }
    if count != findings.len() {
        return Err("capsule policy envelope count does not match its findings".to_owned());
    }
    let kernel = kernel(cache, store)?;
    let mut values = findings
        .iter()
        .map(|(severity, kind)| {
            record_argument(
                &kernel.program,
                "CapsuleFinding",
                vec![
                    ("severity".to_owned(), integer_argument(*severity)),
                    ("kind".to_owned(), integer_argument(*kind)),
                ],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    while values.len() < CAPSULE_ENVELOPE_CAPACITY {
        values.push(record_argument(
            &kernel.program,
            "CapsuleFinding",
            vec![
                ("severity".to_owned(), integer_argument(0)),
                ("kind".to_owned(), integer_argument(0)),
            ],
        )?);
    }
    let arguments = vec![
        ExecutionValue::Sequence {
            values: Arc::new(values),
        },
        ExecutionValue::Byte {
            value: count as i128,
        },
    ];
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: CAPSULE_MODULE.to_owned(),
            function: CAPSULE_FUNCTION.to_owned(),
        },
        arguments,
        type_arguments: Vec::new(),
        step_budget: CAPSULE_STEP_BUDGET,
        policy: Default::default(),
        host_grants: Vec::new(),
        call_depth_budget: None,
    };
    let execution = execute_backend(&kernel.artifact, &request);
    if execution.status != ExecutionStatus::Returned || execution.returned.len() != 1 {
        let reason = execution
            .failure
            .map(|failure| failure.reason)
            .unwrap_or_else(|| format!("execution status was {:?}", execution.status));
        return Err(format!("capsule policy execution refused: {reason}"));
    }
    let ExecutionValue::Record {
        type_identity,
        fields,
        ..
    } = &execution.returned[0]
    else {
        return Err("capsule policy returned a non-record value".to_owned());
    };
    let record = kernel
        .program
        .record_types
        .iter()
        .find(|record| &record.identity == type_identity && record.name == "CapsuleSelection")
        .ok_or_else(|| "capsule policy returned an unknown selection record".to_owned())?;
    if record.fields.len() != fields.len()
        || record
            .fields
            .iter()
            .zip(fields.iter())
            .any(|(expected, actual)| expected.name != actual.0)
    {
        return Err("capsule policy returned an unexpected selection shape".to_owned());
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("capsule selection omitted field {name:?}"))
    };
    let admitted = bool_sequence_field(field("admitted")?, "admitted")?;
    let relevance = integer_sequence_field(field("relevance")?, "relevance")?;
    let admitted_count = integer_field(field("admitted_count")?, "admitted_count")?;
    let actionable_count = integer_field(field("actionable_count")?, "actionable_count")?;
    let watch_count = integer_field(field("watch_count")?, "watch_count")?;
    let dropped_count = integer_field(field("dropped_count")?, "dropped_count")?;
    let complete = bool_field(field("complete")?, "complete")?;
    let valid = bool_field(field("valid")?, "valid")?;
    Ok(CapsuleSelection {
        admitted,
        relevance,
        admitted_count: admitted_count as usize,
        actionable_count: actionable_count as usize,
        watch_count: watch_count as usize,
        dropped_count: dropped_count as usize,
        complete,
        valid,
        kernel_source_identity: kernel.source_identity.clone(),
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
        backend: kernel.artifact.backend.name.clone(),
    })
}

// ---------------------------------------------------------------------------
// Resident queries
// ---------------------------------------------------------------------------

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "error" => 2,
        "warning" => 1,
        _ => 0,
    }
}

fn severity_label(rank: i64) -> &'static str {
    match rank {
        2 => "error",
        1 => "warning",
        _ => "info",
    }
}

fn kind_label(rank: i64) -> &'static str {
    match rank {
        0 => "diagnostic",
        1 => "changed_subject",
        2 => "obligation",
        _ => "affected_module",
    }
}

fn relevance_label(rank: i64) -> &'static str {
    match rank {
        2 => "actionable",
        1 => "watch",
        _ => "informational",
    }
}

fn module_name_for_uri(uri: &str) -> String {
    uri.rsplit('/')
        .next()
        .unwrap_or(uri)
        .strip_suffix(".mncs")
        .unwrap_or(uri)
        .to_owned()
}

impl crate::queries::LanguageService {
    /// Resident status for ambient consumers: identity-bound, bounded, and
    /// cheap on quiet generations (analysis cache reuse, no event replay).
    pub fn service_status(&self) -> Result<ServiceStatusResponse, ServiceError> {
        let workspace_root = self.store.workspace_root_path();
        let mut unresolved = Vec::new();
        let mut diagnostics = DiagnosticTotals::default();
        let mut obligations = ObligationTotals::default();
        let uris = self.store.document_uris();
        for uri in &uris {
            match self.snapshot(uri) {
                Ok(snapshot) => {
                    for diagnostic in snapshot.diagnostics() {
                        match severity_rank(&format!("{:?}", diagnostic.severity).to_lowercase()) {
                            2 => diagnostics.error += 1,
                            1 => diagnostics.warning += 1,
                            _ => {}
                        }
                    }
                    if let Some(program) = snapshot.front_end.program.as_ref() {
                        for obligation in program.generate_obligations().obligations.iter() {
                            match obligation.status {
                                mncs_model::ObligationStatus::Pass => {
                                    obligations.pass += 1;
                                }
                                mncs_model::ObligationStatus::Fail => {
                                    obligations.fail += 1;
                                }
                                mncs_model::ObligationStatus::Unknown => {
                                    obligations.unknown += 1;
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    unresolved.push(format!("snapshot unavailable for {uri}"));
                }
            }
        }
        let mut reasons = Vec::new();
        let ready = if workspace_root.is_none() {
            reasons.push("workspace root is not configured".to_owned());
            false
        } else {
            true
        };
        let toolchain = ToolchainIdentity::current();
        let (executable, build_fingerprint) = build_fingerprint();
        Ok(ServiceStatusResponse {
            schema_version: SERVICE_STATUS_SCHEMA_VERSION.to_owned(),
            service: ServiceIdentity {
                name: "mnls-language-service".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                pid: std::process::id(),
                instance_id: self.instance_id().to_owned(),
                executable,
                build_fingerprint,
            },
            workspace_root,
            toolchain_digest: toolchain.digest(),
            toolchain,
            generation: self.store.generation(),
            stream_identity: self.events.stream_identity(),
            event_cursor: self.events.current_cursor(),
            documents: uris.len(),
            diagnostics,
            obligations,
            readiness: ReadinessState { ready, reasons },
            features: AMBIENT_FEATURES
                .iter()
                .map(|name| name.to_string())
                .collect(),
            checkpoint: self.checkpoint_observation(),
            unresolved,
        })
    }

    /// Bounded semantic capsule over current resident state plus an optional
    /// event window. When `known_stream_identity` names the live stream,
    /// changed subjects and affected modules are measured from events after
    /// `known_cursor`; otherwise the window is reconciled to current state
    /// and `window_matched` is false. Admission is decided by the MNCS
    /// policy kernel; this method only measures and applies.
    pub fn semantic_capsule(
        &self,
        known_stream_identity: Option<&str>,
        known_cursor: u64,
    ) -> Result<SemanticCapsuleResponse, ServiceError> {
        let live_stream = self.events.stream_identity();
        let current_cursor = self.events.current_cursor();
        let window = self.poll_events_for(known_stream_identity, known_cursor, 64);
        // A cursor is only meaningful with its stream identity. A mismatch
        // reconciles to current state explicitly instead of replaying stale
        // deltas or skipping silently.
        let window_matched = !window.reset_required
            && window.stream_identity == live_stream
            && known_stream_identity == Some(live_stream.as_str());
        let mut limitations = Vec::new();
        if window.reset_required {
            limitations.push(
                "event window did not resume from the supplied cursor; capsule measures current state"
                    .to_owned(),
            );
        }

        let mut measured = MeasuredTotals::default();
        let mut unresolved = Vec::new();

        // Current diagnostics across every resident snapshot.
        let mut diagnostic_rows: Vec<(i64, String, MeasuredFinding)> = Vec::new();
        for uri in self.store.document_uris() {
            let response = match self.document_diagnostics(&uri) {
                Ok(response) => response,
                Err(_) => {
                    unresolved.push(format!("diagnostics unavailable for {uri}"));
                    continue;
                }
            };
            for item in response.items {
                let rank = severity_rank(&item.severity);
                measured.diagnostics += 1;
                let key = format!("{}|{}|{}", item.code, uri, item.message);
                diagnostic_rows.push((
                    rank,
                    key.clone(),
                    MeasuredFinding {
                        severity_rank: rank,
                        kind_rank: 0,
                        key,
                        summary: format!("{} {}: {}", item.severity, item.code, item.message),
                        uri: Some(uri.clone()),
                        identity: None,
                        code: Some(item.code.clone()),
                    },
                ));
            }
        }

        // Changed subjects and affected modules from the resumed window only.
        let mut subject_rows: BTreeMap<String, MeasuredFinding> = BTreeMap::new();
        let mut module_rows: BTreeMap<String, MeasuredFinding> = BTreeMap::new();
        if window_matched {
            for event in &window.events {
                for subject in &event.semantic_subjects {
                    let rank = match subject.change.as_str() {
                        "removed" => 2,
                        "changed" => 1,
                        _ => 0,
                    };
                    subject_rows
                        .entry(subject.identity.clone())
                        .and_modify(|existing| {
                            existing.severity_rank = existing.severity_rank.max(rank);
                        })
                        .or_insert_with(|| MeasuredFinding {
                            severity_rank: rank,
                            kind_rank: 1,
                            key: subject.identity.clone(),
                            summary: format!("subject {} {}", subject.identity, subject.change),
                            uri: Some(event.current.uri.clone()),
                            identity: Some(subject.identity.clone()),
                            code: None,
                        });
                }
                let window_severity = if event.diagnostics.added.is_empty() {
                    0
                } else {
                    1
                };
                for document in &event.affected_documents {
                    module_rows
                        .entry(document.uri.clone())
                        .and_modify(|existing: &mut MeasuredFinding| {
                            existing.severity_rank = existing.severity_rank.max(window_severity);
                        })
                        .or_insert_with(|| MeasuredFinding {
                            severity_rank: window_severity,
                            kind_rank: 3,
                            key: document.uri.clone(),
                            summary: format!(
                                "module {} affected by generation {}",
                                module_name_for_uri(&document.uri),
                                event.current_generation
                            ),
                            uri: Some(document.uri.clone()),
                            identity: Some(document.identity.clone()),
                            code: None,
                        });
                }
            }
        }
        measured.changed_subjects = subject_rows.len();
        measured.affected_modules = module_rows.len();

        // Non-pass obligations across every resident snapshot.
        let mut obligation_rows: Vec<MeasuredFinding> = Vec::new();
        for uri in self.store.document_uris() {
            let response = match self.obligations(&uri, None) {
                Ok(response) => response,
                Err(_) => {
                    unresolved.push(format!("obligations unavailable for {uri}"));
                    continue;
                }
            };
            for obligation in response.obligations {
                let rank = match obligation.status.as_str() {
                    "fail" => 2,
                    "unknown" => 1,
                    _ => continue,
                };
                measured.obligations += 1;
                obligation_rows.push(MeasuredFinding {
                    severity_rank: rank,
                    kind_rank: 2,
                    key: obligation.identity.clone(),
                    summary: format!(
                        "obligation {} is {}",
                        obligation.identity, obligation.status
                    ),
                    uri: Some(uri.clone()),
                    identity: Some(obligation.identity.clone()),
                    code: None,
                });
            }
        }

        // Deterministic priority order shared with the policy envelope:
        // severity rank, then kind rank, then identity key.
        let mut envelope: Vec<MeasuredFinding> = Vec::new();
        envelope.extend(diagnostic_rows.into_iter().map(|(_, _, finding)| finding));
        envelope.extend(subject_rows.into_values());
        envelope.extend(obligation_rows);
        envelope.extend(module_rows.into_values());
        envelope.sort_by(|left, right| {
            right
                .severity_rank
                .cmp(&left.severity_rank)
                .then(left.kind_rank.cmp(&right.kind_rank))
                .then(left.key.cmp(&right.key))
        });
        if envelope.len() > CAPSULE_ENVELOPE_CAPACITY {
            measured.truncated_by_envelope = envelope.len() - CAPSULE_ENVELOPE_CAPACITY;
            envelope.truncate(CAPSULE_ENVELOPE_CAPACITY);
            limitations.push(format!(
                "{} measured findings exceed the fixed policy envelope and are counted, not admitted",
                measured.truncated_by_envelope
            ));
        }

        let ranks: Vec<(i64, i64)> = envelope
            .iter()
            .map(|finding| (finding.severity_rank, finding.kind_rank))
            .collect();
        let selection =
            execute_capsule_selection(&self.capsule_kernel, &self.store, &ranks, ranks.len());
        let selection = match selection {
            Ok(selection) => selection,
            Err(reason) => {
                return Ok(SemanticCapsuleResponse {
                    schema_version: SEMANTIC_CAPSULE_SCHEMA_VERSION.to_owned(),
                    status: ResponseStatus::Unsupported {
                        reason: format!("capsule admission policy unavailable: {reason}"),
                    },
                    workspace_root: self.store.workspace_root_path(),
                    generation: self.store.generation(),
                    stream_identity: live_stream,
                    after_cursor: known_cursor,
                    current_cursor,
                    window_matched,
                    measured,
                    policy: None,
                    findings: Vec::new(),
                    unresolved: vec![reason],
                    limitations,
                    snapshot: None,
                });
            }
        };
        if !selection.valid || !selection.complete {
            return Ok(SemanticCapsuleResponse {
                schema_version: SEMANTIC_CAPSULE_SCHEMA_VERSION.to_owned(),
                status: ResponseStatus::Unsupported {
                    reason: "capsule admission policy rejected its envelope".to_owned(),
                },
                workspace_root: self.store.workspace_root_path(),
                generation: self.store.generation(),
                stream_identity: live_stream,
                after_cursor: known_cursor,
                current_cursor,
                window_matched,
                measured,
                policy: Some(CapsulePolicyEvidence {
                    backend: selection.backend,
                    kernel_source_identity: selection.kernel_source_identity,
                    kernel_artifact_identity: selection.kernel_artifact_identity,
                    admitted: selection.admitted_count,
                    actionable: selection.actionable_count,
                    watch: selection.watch_count,
                    dropped: selection.dropped_count,
                    complete: selection.complete,
                    valid: selection.valid,
                }),
                findings: Vec::new(),
                unresolved: vec![
                    "policy verdict was not complete; findings are withheld, not guessed"
                        .to_owned(),
                ],
                limitations,
                snapshot: None,
            });
        }
        let mut findings = Vec::new();
        for (index, candidate) in envelope.iter().enumerate() {
            if !selection.admitted.get(index).copied().unwrap_or(false) {
                continue;
            }
            let relevance =
                relevance_label(selection.relevance.get(index).copied().unwrap_or(0)).to_owned();
            let expansion = match candidate.kind_rank {
                0 => ExpansionHandle {
                    method: "document_diagnostics".to_owned(),
                    params: serde_json::json!({"uri": candidate.uri}),
                },
                1 => ExpansionHandle {
                    method: "describe_identity".to_owned(),
                    params: serde_json::json!({
                        "uri": candidate.uri,
                        "identity": candidate.identity,
                    }),
                },
                2 => ExpansionHandle {
                    method: "obligations".to_owned(),
                    params: serde_json::json!({
                        "uri": candidate.uri,
                        "subject_identity": candidate.identity,
                    }),
                },
                _ => ExpansionHandle {
                    method: "document_diagnostics".to_owned(),
                    params: serde_json::json!({"uri": candidate.uri}),
                },
            };
            findings.push(CapsuleFinding {
                kind: kind_label(candidate.kind_rank).to_owned(),
                relevance,
                severity: severity_label(candidate.severity_rank).to_owned(),
                summary: candidate.summary.clone(),
                uri: candidate.uri.clone(),
                identity: candidate.identity.clone(),
                code: candidate.code.clone(),
                expansion,
            });
        }
        Ok(SemanticCapsuleResponse {
            schema_version: SEMANTIC_CAPSULE_SCHEMA_VERSION.to_owned(),
            status: ResponseStatus::Answered,
            workspace_root: self.store.workspace_root_path(),
            generation: self.store.generation(),
            stream_identity: live_stream,
            after_cursor: known_cursor,
            current_cursor,
            window_matched,
            measured,
            policy: Some(CapsulePolicyEvidence {
                backend: selection.backend,
                kernel_source_identity: selection.kernel_source_identity,
                kernel_artifact_identity: selection.kernel_artifact_identity,
                admitted: selection.admitted_count,
                actionable: selection.actionable_count,
                watch: selection.watch_count,
                dropped: selection.dropped_count,
                complete: selection.complete,
                valid: selection.valid,
            }),
            findings,
            unresolved,
            limitations,
            snapshot: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel_selection(findings: &[(i64, i64)]) -> CapsuleSelection {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        execute_capsule_selection(&cache, &store, findings, findings.len())
            .expect("capsule policy executes")
    }

    #[test]
    fn envelope_over_capacity_is_refused_before_execution() {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let findings = vec![(2, 0); CAPSULE_ENVELOPE_CAPACITY + 1];
        let error =
            execute_capsule_selection(&cache, &store, &findings, findings.len()).unwrap_err();
        assert!(error.contains("bounded to 32 findings"), "{error}");
    }

    #[test]
    fn errors_admit_and_informationals_never_do() {
        // (severity, kind): error diagnostic, info diagnostic, removed
        // subject, added subject, failing obligation, quiet module.
        let selection = kernel_selection(&[(2, 0), (0, 0), (2, 1), (0, 1), (2, 2), (0, 3)]);
        assert!(selection.valid, "valid envelope");
        assert!(selection.complete, "every active finding is decided");
        assert_eq!(selection.relevance[0], 2, "error diagnostic is actionable");
        assert_eq!(selection.relevance[1], 0, "info diagnostic is context only");
        assert_eq!(selection.relevance[2], 2, "removed subject is actionable");
        assert_eq!(selection.relevance[3], 1, "added subject is watch");
        assert_eq!(
            selection.relevance[4], 2,
            "failing obligation is actionable"
        );
        assert_eq!(selection.relevance[5], 0, "quiet module is context only");
        assert!(selection.admitted[0]);
        assert!(!selection.admitted[1], "informational is never admitted");
        assert!(selection.admitted[2]);
        assert!(selection.admitted[3]);
        assert!(selection.admitted[4]);
        assert!(!selection.admitted[5], "informational is never admitted");
        assert_eq!(selection.admitted_count, 4);
        assert_eq!(selection.actionable_count, 3);
        assert_eq!(selection.watch_count, 1);
        assert_eq!(selection.dropped_count, 2);
        assert_eq!(selection.backend, "mncs-research-bytecode");
        assert!(!selection.kernel_artifact_identity.is_empty());
    }

    #[test]
    fn budgets_bind_and_overflow_is_counted() {
        // 20 actionable error diagnostics; the policy admits at most
        // max_actionable (16) within max_total (20).
        let findings = vec![(2, 0); 20];
        let selection = kernel_selection(&findings);
        assert!(selection.valid);
        assert!(selection.complete);
        assert_eq!(selection.actionable_count, 16);
        assert_eq!(selection.admitted_count, 16);
        assert_eq!(selection.dropped_count, 4);
        let admitted: Vec<bool> = selection.admitted.into_iter().take(20).collect();
        assert!(admitted.iter().take(16).all(|flag| *flag));
        assert!(admitted.iter().skip(16).all(|flag| !flag));
    }

    #[test]
    fn empty_envelope_is_valid_and_complete() {
        let selection = kernel_selection(&[]);
        assert!(selection.valid);
        assert!(selection.complete);
        assert_eq!(selection.admitted_count, 0);
        assert_eq!(selection.dropped_count, 0);
    }

    #[test]
    fn toolchain_digest_is_stable_and_sensitive() {
        let first = ToolchainIdentity {
            language_root: Some("/repo/mncs-language".to_owned()),
            library_path: Some("/repo/mncs-language/library".to_owned()),
            pinned: None,
        };
        let second = first.clone();
        assert_eq!(first.digest(), second.digest());
        let changed = ToolchainIdentity {
            pinned: Some("rev-2".to_owned()),
            ..first.clone()
        };
        assert_ne!(first.digest(), changed.digest());
    }
}
