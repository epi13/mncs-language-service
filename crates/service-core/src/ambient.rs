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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

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
use crate::queries::{render_diagnostics, ResponseStatus, SnapshotInfo};

pub const SERVICE_STATUS_SCHEMA_VERSION: &str = "mncs.language-service.service-status/1";
pub const SEMANTIC_CAPSULE_SCHEMA_VERSION: &str = "mncs.language-service.semantic-capsule/1";
pub const SEMANTIC_IMPACT_SCHEMA_VERSION: &str = "mncs.language-service.semantic-impact/1";

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
    /// SHA-256 identity of the executable bytes loaded by this process.
    /// The path is reported separately and is never part of build identity.
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

fn file_fingerprint(path: &std::path::Path) -> Option<String> {
    use std::io::Read;

    let mut image = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = image.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn build_fingerprint() -> (Option<String>, String) {
    let executable = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    let fingerprint = FINGERPRINT.get_or_init(|| {
        // On Linux this names the image actually loaded by this process,
        // including when the path was replaced after startup. Other targets
        // fall back to current_exe. Failure remains explicitly unknown.
        file_fingerprint(std::path::Path::new("/proc/self/exe"))
            .or_else(|| {
                executable
                    .as_deref()
                    .and_then(|path| file_fingerprint(std::path::Path::new(path)))
            })
            .unwrap_or_else(|| "unknown".to_owned())
    });
    (executable, fingerprint.clone())
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
    /// Exact roots included in resident source discovery. An Environment
    /// selection change creates a new semantic stream epoch.
    #[serde(default)]
    pub workspace_repository_roots: Vec<String>,
    pub toolchain: ToolchainIdentity,
    pub toolchain_digest: String,
    pub generation: u64,
    pub stream_identity: String,
    pub event_cursor: u64,
    pub documents: usize,
    /// Discovered documents without a current cached analysis. Readiness
    /// probes report this instead of compiling every source document.
    #[serde(default)]
    pub analysis_pending: usize,
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
#[serde(default)]
pub struct MeasuredTotals {
    pub diagnostics: usize,
    pub changed_subjects: usize,
    pub obligations: usize,
    pub affected_modules: usize,
    /// Resident documents without a current semantic analysis. Capsules do
    /// not trigger workspace compilation to fill this gap.
    pub analysis_pending_documents: usize,
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
        let unresolved = Vec::new();
        let mut diagnostics = DiagnosticTotals::default();
        let mut obligations = ObligationTotals::default();
        let uris = self.store.document_uris();
        let mut analysis_pending = 0;
        for uri in &uris {
            match self.cached_snapshot_if_current(uri) {
                Some(snapshot) => {
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
                None => analysis_pending += 1,
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
        let workspace_repository_roots = self
            .store
            .discovery_roots()
            .iter()
            .map(|path| path.display().to_string())
            .collect();
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
            workspace_repository_roots,
            toolchain_digest: toolchain.digest(),
            toolchain,
            generation: self.store.generation(),
            stream_identity: self.events.stream_identity(),
            event_cursor: self.events.current_cursor(),
            documents: uris.len(),
            analysis_pending,
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
        let unresolved = Vec::new();

        // Measure only snapshots already resident and current. This endpoint
        // is used by bounded ambient consumers; turning a cursor recovery
        // call into analysis of every selected workspace document can outlive
        // the caller's deadline while retaining a runaway compiler job.
        let mut diagnostic_rows: Vec<(i64, String, MeasuredFinding)> = Vec::new();
        for uri in self.store.document_uris() {
            let Some(snapshot) = self.cached_snapshot_if_current(&uri) else {
                measured.analysis_pending_documents += 1;
                continue;
            };
            for item in render_diagnostics(&snapshot, &self.store) {
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
            let Some(snapshot) = self.cached_snapshot_if_current(&uri) else {
                continue;
            };
            let Some(program) = snapshot.front_end.program.as_ref() else {
                continue;
            };
            for obligation in program.generate_obligations().obligations {
                let rank = match &obligation.status {
                    mncs_model::ObligationStatus::Fail => 2,
                    mncs_model::ObligationStatus::Unknown => 1,
                    _ => continue,
                };
                measured.obligations += 1;
                let identity = obligation.identity.0.clone();
                obligation_rows.push(MeasuredFinding {
                    severity_rank: rank,
                    kind_rank: 2,
                    key: identity.clone(),
                    summary: format!(
                        "obligation {} is {}",
                        identity,
                        match obligation.status {
                            mncs_model::ObligationStatus::Pass => "pass",
                            mncs_model::ObligationStatus::Fail => "fail",
                            mncs_model::ObligationStatus::Unknown => "unknown",
                        }
                    ),
                    uri: Some(uri.clone()),
                    identity: Some(identity),
                    code: None,
                });
            }
        }

        if measured.analysis_pending_documents > 0 {
            let pending = measured.analysis_pending_documents;
            return Ok(SemanticCapsuleResponse {
                schema_version: SEMANTIC_CAPSULE_SCHEMA_VERSION.to_owned(),
                status: ResponseStatus::Unsupported {
                    reason: format!(
                        "semantic capsule is incomplete: {pending} resident documents lack current cached analysis"
                    ),
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
                unresolved: vec!["current workspace semantics are incomplete".to_owned()],
                limitations,
                snapshot: None,
            });
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

/// Structured semantic impact for one subject: direct dependency edges,
// the bounded compiler-owned impact neighborhood, and the obligations
// whose subjects fall inside the affected set. This publishes semantic
// facts; verification owners decide what the facts invalidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticImpactResponse {
    pub schema_version: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
    pub subject_identity: String,
    pub dependencies: crate::queries::GraphResponse,
    pub dependents: crate::queries::GraphResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<mncs_model::SemanticImpact>,
    pub impact_complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_obligations: Vec<crate::queries::ObligationInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl crate::queries::LanguageService {
    /// On-demand impact for any resolved identity: single-hop edges plus
    /// the bounded neighborhood (depth 2, 256 nodes) and affected
    /// obligations. Mirrors the event-path impact projection so ambient
    /// consumers and verification owners share one vocabulary.
    pub fn semantic_impact(
        &self,
        uri: &str,
        identity: &str,
    ) -> Result<SemanticImpactResponse, ServiceError> {
        let snapshot = self.snapshot(uri)?;
        let info = crate::queries::snapshot_info(uri, &snapshot);
        let dependencies = self.graph_query(uri, identity, true)?;
        let dependents = self.graph_query(uri, identity, false)?;
        let mut limitations = Vec::new();
        let mut impact = None;
        let mut impact_complete = false;
        let mut affected: BTreeSet<String> = BTreeSet::from([identity.to_owned()]);
        if let Some(program) = snapshot.front_end.program.as_ref() {
            match program.semantic_graph() {
                Ok(graph) => {
                    let projected = graph.impact_neighborhood(
                        &[mncs_model::SemanticId(identity.to_owned())],
                        2,
                        256,
                    );
                    impact_complete = projected.complete;
                    for node in &projected.nodes {
                        affected.insert(node.identity.0.clone());
                    }
                    for edge in &projected.edges {
                        affected.insert(edge.from.0.clone());
                        affected.insert(edge.to.0.clone());
                    }
                    impact = Some(projected);
                }
                Err(_) => {
                    limitations
                        .push("the current program could not produce a semantic graph".to_owned());
                }
            }
        } else {
            limitations.push("impact neighborhood requires a valid elaborated program".to_owned());
        }
        let mut affected_obligations = Vec::new();
        match self.obligations(uri, None) {
            Ok(response) => {
                for obligation in response.obligations {
                    if affected.contains(&obligation.subject)
                        || affected.contains(&obligation.identity)
                    {
                        affected_obligations.push(obligation);
                    }
                }
            }
            Err(_) => {
                limitations.push("obligations unavailable for this snapshot".to_owned());
            }
        }
        affected_obligations.sort_by(|left, right| left.identity.cmp(&right.identity));
        Ok(SemanticImpactResponse {
            schema_version: SEMANTIC_IMPACT_SCHEMA_VERSION.to_owned(),
            status: ResponseStatus::Answered,
            snapshot: Some(info),
            subject_identity: identity.to_owned(),
            dependencies,
            dependents,
            impact,
            impact_complete,
            affected_obligations,
            limitations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mnls-selected-roots-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("temporary workspace");
            Self(path)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn kernel_selection(findings: &[(i64, i64)]) -> CapsuleSelection {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        execute_capsule_selection(&cache, &store, findings, findings.len())
            .expect("capsule policy executes")
    }

    #[test]
    fn streamed_build_fingerprint_matches_whole_image_sha256() {
        use sha2::Digest;

        let workspace = TempWorkspace::new();
        let path = workspace.0.join("service-image.bin");
        let image: Vec<u8> = (0..1_000_003).map(|index| (index % 251) as u8).collect();
        fs::write(&path, &image).expect("write fingerprint fixture");
        let expected = format!("sha256:{:x}", Sha256::digest(&image));
        assert_eq!(file_fingerprint(&path).as_deref(), Some(expected.as_str()));
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

    #[test]
    fn status_is_lazy_and_selected_root_changes_start_a_new_stream() {
        let workspace = TempWorkspace::new();
        let root = &workspace.0;
        let selected = root.join("selected");
        let other = root.join("other");
        fs::create_dir_all(&selected).expect("selected root");
        fs::create_dir_all(&other).expect("other root");
        fs::write(selected.join("selected.mncs"), "mncs 0.2;\n").expect("selected source");
        fs::write(other.join("unselected.mncs"), "mncs 0.2;\n").expect("unselected source");

        let first = crate::queries::LanguageService::new(Some(root.clone()));
        let initial_roots = vec![selected.clone(), other.clone()];
        first
            .configure_root_with_discovery_roots(Some(root.clone()), Some(initial_roots))
            .expect("configure selected root");
        let first_status = first.service_status().expect("lazy service status");
        assert_eq!(first_status.documents, 2);
        assert_eq!(first_status.analysis_pending, 2);
        let first_stream = first_status.stream_identity;
        drop(first);

        let second = crate::queries::LanguageService::new(Some(root.clone()));
        second
            .configure_root_with_discovery_roots(Some(root.clone()), Some(vec![other.clone()]))
            .expect("reconfigure selected root");
        let second_status = second.service_status().expect("second lazy status");
        assert_eq!(second_status.documents, 1);
        assert_eq!(second_status.analysis_pending, 1);
        assert_eq!(
            second_status.workspace_repository_roots,
            vec![other.display().to_string()]
        );
        assert_ne!(
            first_stream, second_status.stream_identity,
            "a selected repository-set change cannot resume the old semantic stream"
        );
    }
}
