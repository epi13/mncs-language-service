//! MNCS-native bounded summaries for resident call-graph relationships.
//!
//! The adapter assigns numeric ranks to identities, executes ordering and
//! duplicate removal through `mncs.std.sort.v1`, then maps ranks back to the
//! service's protocol identities. Compiler call-edge extraction stays owned
//! by the authoritative elaborated program.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use mncs_codegen::{execute_backend, RESEARCH_BYTECODE_BACKEND_NAME};
use mncs_compiler::{ModuleResolver, ReferenceCompiler, SourceFrontEndResult};
use mncs_model::{
    ArtifactRepresentation, CompilationStatus, ExecutionRequest, ExecutionStatus, ExecutionTarget,
    ExecutionValue, IntegerType, Program, TransformationStatus, EXECUTION_REQUEST_SCHEMA_VERSION,
};

use crate::document::DocumentStore;
use crate::modules::StoreResolver;

pub(crate) const RELATION_MODULE: &str = "mncs.language_service.call_relation_summary.v1";
pub(crate) const RELATION_FUNCTION: &str = "summarize_neighbors";
pub(crate) const RELATION_WINDOW_CAPACITY: usize = 64;
pub(crate) const RELATION_WALK_BATCH_CAPACITY: usize = 64;
pub(crate) const RELATION_WALK_RESULT_CAPACITY: usize = 1024;
pub(crate) const RELATION_WALK_ROOT_CODE: u64 = 1024;
const RELATION_URI: &str = "mncs://language-service/call-relation-summary.mncs";
const RELATION_SOURCE: &str = include_str!("../../../mncs/call_relation_summary.mncs");
const STEP_BUDGET: u64 = 100_000;
const WALK_STEP_BUDGET: u64 = 8_000_000;

#[derive(Debug, Clone)]
pub(crate) struct RelationKernel {
    source_identity: String,
    dependency_identities: BTreeMap<String, String>,
    program: Arc<Program>,
    artifact: Arc<mncs_model::BackendArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeNeighborSummary {
    pub targets: Vec<u64>,
    pub backend: String,
    pub kernel_source_identity: String,
    pub dependency_source_identity: String,
    pub kernel_artifact_identity: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeWalkState {
    value: ExecutionValue,
    pub frontier: Vec<u64>,
    pub reached_count: usize,
    frontier_depth: u64,
    pub truncated: bool,
    pub backend: String,
    pub kernel_source_identity: String,
    pub dependency_source_identity: String,
    pub kernel_artifact_identity: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeWalkStep {
    pub state: NativeWalkState,
    pub accepted: Vec<bool>,
}

fn frontend_error(front_end: &SourceFrontEndResult) -> String {
    let details = front_end
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>();
    if details.is_empty() {
        "MNCS-native relation summary did not produce an authoritative program".to_owned()
    } else {
        format!(
            "MNCS-native relation summary source is unsupported: {}",
            details.join("; ")
        )
    }
}

fn prepare_kernel(store: &DocumentStore, source_identity: &str) -> Result<RelationKernel, String> {
    let resolver = StoreResolver::new(store);
    let envelope = store.envelope(RELATION_URI, RELATION_SOURCE);
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
    let dependency_identities = front_end
        .module_resolutions
        .iter()
        .map(|dependency| {
            (
                dependency.requested_module.clone(),
                dependency.source_identity.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let request = compiler
        .request_for_program_with_backend(
            &program,
            BTreeSet::from([ArtifactRepresentation::BackendArtifact]),
            RESEARCH_BYTECODE_BACKEND_NAME,
        )
        .map_err(|diagnostic| {
            format!(
                "MNCS-native relation summary request refused: {}",
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
            "MNCS-native relation summary compilation failed: {}",
            details.join("; ")
        ));
    }
    let artifact = compilation.emissions.backend.ok_or_else(|| {
        "MNCS-native relation summary backend artifact was not emitted".to_owned()
    })?;
    if artifact.backend.name != RESEARCH_BYTECODE_BACKEND_NAME
        || !artifact.identity_is_valid()
        || artifact.status != TransformationStatus::Pass
    {
        return Err(
            "MNCS-native relation summary artifact failed identity/status validation".to_owned(),
        );
    }
    Ok(RelationKernel {
        source_identity: source_identity.to_owned(),
        dependency_identities,
        program,
        artifact: Arc::new(artifact),
    })
}

fn kernel(
    cache: &RwLock<Option<Arc<RelationKernel>>>,
    store: &DocumentStore,
) -> Result<Arc<RelationKernel>, String> {
    let envelope = store.envelope(RELATION_URI, RELATION_SOURCE);
    let resolver = StoreResolver::new(store);
    let sort_dependency = resolver.resolve("mncs.std.sort.v1").ok_or_else(|| {
        "MNCS-native relation summary requires mncs.std.sort.v1 from MNCS_LIBRARY_PATH".to_owned()
    })?;
    let walk_dependency = resolver
        .resolve("mncs.std.relation_walk.v1")
        .ok_or_else(|| {
            "MNCS-native relation walk requires mncs.std.relation_walk.v1 from MNCS_LIBRARY_PATH"
                .to_owned()
        })?;
    if let Ok(read) = cache.read() {
        if let Some(existing) = read.as_ref() {
            if existing.source_identity == envelope.identity
                && existing.dependency_identities.get("mncs.std.sort.v1")
                    == Some(&sort_dependency.identity)
                && existing
                    .dependency_identities
                    .get("mncs.std.relation_walk.v1")
                    == Some(&walk_dependency.identity)
            {
                return Ok(Arc::clone(existing));
            }
        }
    }
    let prepared = Arc::new(prepare_kernel(store, &envelope.identity)?);
    let mut write = cache
        .write()
        .map_err(|_| "MNCS-native relation summary cache is poisoned".to_owned())?;
    if let Some(existing) = write.as_ref() {
        if existing.source_identity == prepared.source_identity
            && existing.dependency_identities == prepared.dependency_identities
        {
            return Ok(Arc::clone(existing));
        }
    }
    *write = Some(Arc::clone(&prepared));
    Ok(prepared)
}

fn integer_argument(value: u64) -> ExecutionValue {
    ExecutionValue::Integer {
        value: value as i128,
        ty: IntegerType {
            bits: 64,
            signed: false,
        },
    }
}

fn integer_field(value: &ExecutionValue, field: &str) -> Result<u64, String> {
    let ExecutionValue::Integer { value, ty } = value else {
        return Err(format!(
            "native relation summary field {field:?} is not an integer"
        ));
    };
    if *ty
        != (IntegerType {
            bits: 64,
            signed: false,
        })
        || *value < 0
    {
        return Err(format!(
            "native relation summary field {field:?} is invalid"
        ));
    }
    u64::try_from(*value)
        .map_err(|_| format!("native relation summary field {field:?} is too large"))
}

pub(crate) fn execute_neighbor_summary(
    cache: &RwLock<Option<Arc<RelationKernel>>>,
    store: &DocumentStore,
    candidates: &[u64],
) -> Result<NativeNeighborSummary, String> {
    if candidates.len() > RELATION_WINDOW_CAPACITY {
        return Err(format!(
            "MNCS-native relation summary is bounded to {RELATION_WINDOW_CAPACITY} targets; received {}",
            candidates.len()
        ));
    }
    let kernel = kernel(cache, store)?;
    let mut values = candidates
        .iter()
        .copied()
        .map(integer_argument)
        .collect::<Vec<_>>();
    while values.len() < RELATION_WINDOW_CAPACITY {
        values.push(integer_argument(0));
    }
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: RELATION_MODULE.to_owned(),
            function: RELATION_FUNCTION.to_owned(),
        },
        arguments: vec![
            ExecutionValue::Sequence {
                values: Arc::new(values),
            },
            ExecutionValue::Byte {
                value: candidates.len() as i128,
            },
        ],
        type_arguments: Vec::new(),
        step_budget: STEP_BUDGET,
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
        return Err(format!(
            "MNCS-native relation summary execution refused: {reason}"
        ));
    }
    let ExecutionValue::Record {
        type_identity,
        fields,
        ..
    } = &execution.returned[0]
    else {
        return Err("MNCS-native relation summary returned a non-record value".to_owned());
    };
    let record = kernel
        .program
        .record_types
        .iter()
        .find(|record| record.identity == *type_identity && record.name == "NeighborSummary")
        .ok_or_else(|| "MNCS-native relation summary returned an unknown record".to_owned())?;
    if record.fields.len() != fields.len()
        || record
            .fields
            .iter()
            .zip(fields.iter())
            .any(|(expected, actual)| expected.name != actual.0)
    {
        return Err("MNCS-native relation summary returned an unexpected record shape".to_owned());
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("native relation summary omitted field {name:?}"))
    };
    let count = usize::try_from(integer_field(field("count")?, "count")?)
        .map_err(|_| "native relation summary count is too large".to_owned())?;
    let ExecutionValue::Sequence { values } = field("targets")? else {
        return Err("native relation summary targets are not a sequence".to_owned());
    };
    if values.len() != RELATION_WINDOW_CAPACITY || count > candidates.len() || count > values.len()
    {
        return Err("native relation summary returned an invalid target count".to_owned());
    }
    let targets = values[..count]
        .iter()
        .map(|value| integer_field(value, "targets"))
        .collect::<Result<Vec<_>, _>>()?;
    if targets.windows(2).any(|pair| pair[0] >= pair[1])
        || targets.iter().any(|target| !candidates.contains(target))
    {
        return Err("native relation summary returned unordered or invented targets".to_owned());
    }
    let dependency_source_identity = kernel
        .dependency_identities
        .get("mncs.std.sort.v1")
        .cloned()
        .ok_or_else(|| "native relation summary lost its Stdlib dependency identity".to_owned())?;
    Ok(NativeNeighborSummary {
        targets,
        backend: kernel.artifact.backend.name.clone(),
        kernel_source_identity: kernel.source_identity.clone(),
        dependency_source_identity,
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
    })
}

fn record_fields<'a>(
    program: &Program,
    value: &'a ExecutionValue,
    expected_name: &str,
) -> Result<&'a [(String, ExecutionValue)], String> {
    let ExecutionValue::Record {
        type_identity,
        fields,
        ..
    } = value
    else {
        return Err(format!(
            "native relation walk {expected_name} value is not a record"
        ));
    };
    let record = program
        .record_types
        .iter()
        .find(|record| record.identity == *type_identity && record.name == expected_name)
        .ok_or_else(|| {
            format!("native relation walk returned an unknown {expected_name} record")
        })?;
    if record.fields.len() != fields.len()
        || record
            .fields
            .iter()
            .zip(fields.iter())
            .any(|(expected, actual)| expected.name != actual.0)
    {
        return Err(format!(
            "native relation walk returned an unexpected {expected_name} shape"
        ));
    }
    Ok(fields.as_slice())
}

fn record_field<'a>(
    fields: &'a [(String, ExecutionValue)],
    name: &str,
) -> Result<&'a ExecutionValue, String> {
    fields
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("native relation walk record omitted field {name:?}"))
}

fn boolean_field(value: &ExecutionValue, field: &str) -> Result<bool, String> {
    let ExecutionValue::Boolean { value } = value else {
        return Err(format!(
            "native relation walk field {field:?} is not boolean"
        ));
    };
    Ok(*value)
}

fn boolean_sequence_field(
    value: &ExecutionValue,
    field: &str,
    expected_len: usize,
) -> Result<Vec<bool>, String> {
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!(
            "native relation walk field {field:?} is not a sequence"
        ));
    };
    if values.len() != expected_len {
        return Err(format!(
            "native relation walk field {field:?} has length {} instead of {expected_len}",
            values.len()
        ));
    }
    values
        .iter()
        .map(|value| match value {
            ExecutionValue::Boolean { value } => Ok(*value),
            _ => Err(format!(
                "native relation walk field {field:?} is not a boolean sequence"
            )),
        })
        .collect()
}

fn integer_sequence_field(
    value: &ExecutionValue,
    field: &str,
    expected_len: usize,
) -> Result<Vec<u64>, String> {
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!(
            "native relation walk field {field:?} is not a sequence"
        ));
    };
    if values.len() != expected_len {
        return Err(format!(
            "native relation walk field {field:?} has length {} instead of {expected_len}",
            values.len()
        ));
    }
    values
        .iter()
        .map(|value| integer_field(value, field))
        .collect()
}

struct DecodedWalkState {
    frontier: Vec<u64>,
    reached_count: usize,
    frontier_depth: u64,
    truncated: bool,
}

fn decode_walk_state(
    program: &Program,
    value: &ExecutionValue,
) -> Result<DecodedWalkState, String> {
    let fields = record_fields(program, value, "WalkState")?;
    let seen = boolean_sequence_field(
        record_field(fields, "seen")?,
        "seen",
        RELATION_WALK_RESULT_CAPACITY,
    )?;
    let root_seen = boolean_field(record_field(fields, "root_seen")?, "root_seen")?;
    let frontier_values = integer_sequence_field(
        record_field(fields, "frontier")?,
        "frontier",
        RELATION_WALK_RESULT_CAPACITY,
    )?;
    let frontier_count = usize::try_from(integer_field(
        record_field(fields, "frontier_count")?,
        "frontier_count",
    )?)
    .map_err(|_| "native relation walk frontier count is too large".to_owned())?;
    let reached_count = usize::try_from(integer_field(
        record_field(fields, "reached_count")?,
        "reached_count",
    )?)
    .map_err(|_| "native relation walk reached count is too large".to_owned())?;
    let frontier_depth = integer_field(record_field(fields, "frontier_depth")?, "frontier_depth")?;
    let truncated = boolean_field(record_field(fields, "truncated")?, "truncated")?;
    let valid = boolean_field(record_field(fields, "valid")?, "valid")?;

    let seen_count = seen.iter().filter(|seen| **seen).count();
    if !valid
        || !root_seen
        || frontier_count > RELATION_WALK_RESULT_CAPACITY
        || !(1..=RELATION_WALK_RESULT_CAPACITY + 1).contains(&reached_count)
        || frontier_depth > 16
        || seen_count + 1 != reached_count
    {
        return Err(format!(
            "native relation walk returned invalid bounded state (valid={valid}, root_seen={root_seen}, frontier_count={frontier_count}, reached_count={reached_count}, frontier_depth={frontier_depth}, seen_count={seen_count})"
        ));
    }
    let frontier = frontier_values[..frontier_count].to_vec();
    let mut unique_frontier = BTreeSet::new();
    for code in &frontier {
        let is_root = *code == RELATION_WALK_ROOT_CODE;
        if *code > RELATION_WALK_ROOT_CODE
            || (!is_root && !seen[*code as usize])
            || !unique_frontier.insert(*code)
        {
            return Err("native relation walk returned an invalid frontier code".to_owned());
        }
    }
    Ok(DecodedWalkState {
        frontier,
        reached_count,
        frontier_depth,
        truncated,
    })
}

fn native_walk_state(
    value: ExecutionValue,
    kernel: &RelationKernel,
) -> Result<NativeWalkState, String> {
    let decoded = decode_walk_state(&kernel.program, &value)?;
    let dependency_source_identity = kernel
        .dependency_identities
        .get("mncs.std.relation_walk.v1")
        .cloned()
        .ok_or_else(|| "native relation walk lost its Stdlib dependency identity".to_owned())?;
    Ok(NativeWalkState {
        value,
        frontier: decoded.frontier,
        reached_count: decoded.reached_count,
        frontier_depth: decoded.frontier_depth,
        truncated: decoded.truncated,
        backend: kernel.artifact.backend.name.clone(),
        kernel_source_identity: kernel.source_identity.clone(),
        dependency_source_identity,
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
    })
}

fn execute_relation_walk_function(
    kernel: &RelationKernel,
    function: &str,
    arguments: Vec<ExecutionValue>,
) -> Result<ExecutionValue, String> {
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: RELATION_MODULE.to_owned(),
            function: function.to_owned(),
        },
        arguments,
        type_arguments: Vec::new(),
        step_budget: WALK_STEP_BUDGET,
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
        return Err(format!("MNCS-native relation walk refused: {reason}"));
    }
    Ok(execution.returned[0].clone())
}

pub(crate) fn execute_walk_start(
    cache: &RwLock<Option<Arc<RelationKernel>>>,
    store: &DocumentStore,
) -> Result<NativeWalkState, String> {
    let kernel = kernel(cache, store)?;
    let value = execute_relation_walk_function(
        &kernel,
        "start_relation_walk",
        vec![integer_argument(RELATION_WALK_ROOT_CODE)],
    )?;
    native_walk_state(value, &kernel)
}

pub(crate) fn execute_walk_advance_batch(
    cache: &RwLock<Option<Arc<RelationKernel>>>,
    store: &DocumentStore,
    state: &NativeWalkState,
    candidates: &[u64],
    depth: usize,
    max_nodes: usize,
) -> Result<NativeWalkStep, String> {
    if candidates.len() > RELATION_WALK_BATCH_CAPACITY {
        return Err(format!(
            "MNCS-native relation walk accepts at most {} candidates per batch; received {}",
            RELATION_WALK_BATCH_CAPACITY,
            candidates.len()
        ));
    }
    if !(1..=16).contains(&depth) {
        return Err("MNCS-native relation walk depth must be in 1..=16".to_owned());
    }
    if max_nodes > RELATION_WALK_RESULT_CAPACITY {
        return Err(format!(
            "MNCS-native relation walk result budget exceeds {} nodes",
            RELATION_WALK_RESULT_CAPACITY
        ));
    }
    if candidates
        .iter()
        .any(|candidate| *candidate > RELATION_WALK_ROOT_CODE)
    {
        return Err("MNCS-native relation walk received an out-of-range node code".to_owned());
    }
    let kernel = kernel(cache, store)?;
    if state.backend != kernel.artifact.backend.name
        || state.kernel_source_identity != kernel.source_identity
        || state.kernel_artifact_identity != kernel.artifact.identity.0
        || state.dependency_source_identity
            != kernel
                .dependency_identities
                .get("mncs.std.relation_walk.v1")
                .map(String::as_str)
                .unwrap_or_default()
    {
        return Err("MNCS-native relation walk state belongs to a stale kernel".to_owned());
    }
    let mut candidate_values = candidates
        .iter()
        .copied()
        .map(integer_argument)
        .collect::<Vec<_>>();
    while candidate_values.len() < RELATION_WALK_BATCH_CAPACITY {
        candidate_values.push(integer_argument(0));
    }
    let value = execute_relation_walk_function(
        &kernel,
        "advance_relation_walk",
        vec![
            state.value.clone(),
            ExecutionValue::Sequence {
                values: Arc::new(candidate_values),
            },
            integer_argument(candidates.len() as u64),
            integer_argument(depth as u64),
            integer_argument(max_nodes as u64),
        ],
    )?;
    let fields = record_fields(&kernel.program, &value, "AdvanceOut")?;
    let state_value = record_field(fields, "state")?.clone();
    let new_state = native_walk_state(state_value, &kernel)?;
    let accepted_mask = boolean_sequence_field(
        record_field(fields, "accepted")?,
        "accepted",
        RELATION_WALK_BATCH_CAPACITY,
    )?;
    let accepted_count = usize::try_from(integer_field(
        record_field(fields, "accepted_count")?,
        "accepted_count",
    )?)
    .map_err(|_| "native relation walk accepted count is too large".to_owned())?;
    if accepted_mask[candidates.len()..]
        .iter()
        .any(|accepted| *accepted)
        || accepted_mask[..candidates.len()]
            .iter()
            .filter(|accepted| **accepted)
            .count()
            != accepted_count
        || new_state.reached_count != state.reached_count + accepted_count
        || new_state.frontier_depth != depth as u64
        || new_state.frontier.len()
            != (if state.frontier_depth == depth as u64 {
                state.frontier.len()
            } else {
                0
            } + accepted_count)
        || accepted_mask[..candidates.len()]
            .iter()
            .zip(candidates)
            .any(|(accepted, code)| *accepted && *code == RELATION_WALK_ROOT_CODE)
    {
        return Err("MNCS-native relation walk returned inconsistent admission state".to_owned());
    }
    let accepted_codes = accepted_mask[..candidates.len()]
        .iter()
        .zip(candidates)
        .filter_map(|(accepted, code)| accepted.then_some(*code))
        .collect::<Vec<_>>();
    if accepted_codes
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .len()
        != accepted_count
    {
        return Err("MNCS-native relation walk accepted a duplicate node code".to_owned());
    }
    Ok(NativeWalkStep {
        state: new_state,
        accepted: accepted_mask[..candidates.len()].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> std::path::PathBuf {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(root) = std::env::var_os("MNCS_STDLIB_ROOT") {
            let library = std::path::PathBuf::from(root).join("library");
            if library.join("std/sort.mncs").is_file() {
                return library;
            }
        }
        [
            manifest_dir.join("../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../../mncs-stdlib/library"),
        ]
        .into_iter()
        .find(|path| path.join("std/sort.mncs").is_file())
        .expect("sibling MNCS standard library")
    }

    #[test]
    fn native_summary_orders_and_deduplicates_direct_neighbor_ranks() {
        std::env::set_var("MNCS_LIBRARY_PATH", library());
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let expected_dependency_identity = StoreResolver::new(&store)
            .resolve("mncs.std.sort.v1")
            .expect("current Stdlib sort module")
            .identity;
        let candidates = [7, 2, 7, 5, 2, 1, 5];
        let summary =
            execute_neighbor_summary(&cache, &store, &candidates).expect("native relation summary");
        assert_eq!(summary.targets, vec![1, 2, 5, 7]);
        assert_eq!(summary.backend, RESEARCH_BYTECODE_BACKEND_NAME);
        assert!(!summary.kernel_source_identity.is_empty());
        assert_eq!(
            summary.dependency_source_identity,
            expected_dependency_identity
        );
        assert!(!summary.kernel_artifact_identity.is_empty());
    }

    #[test]
    fn native_summary_handles_empty_and_full_windows() {
        std::env::set_var("MNCS_LIBRARY_PATH", library());
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        assert!(execute_neighbor_summary(&cache, &store, &[])
            .expect("empty relation summary")
            .targets
            .is_empty());

        let candidates = (0..RELATION_WINDOW_CAPACITY)
            .rev()
            .map(|value| value as u64)
            .collect::<Vec<_>>();
        let summary =
            execute_neighbor_summary(&cache, &store, &candidates).expect("full relation summary");
        assert_eq!(
            summary.targets,
            (0..RELATION_WINDOW_CAPACITY as u64).collect::<Vec<u64>>()
        );
    }

    #[test]
    fn native_summary_refuses_candidates_over_its_fixed_envelope() {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let error =
            execute_neighbor_summary(&cache, &store, &vec![0; RELATION_WINDOW_CAPACITY + 1])
                .unwrap_err();
        assert!(error.contains("bounded to 64 targets"));
    }

    #[test]
    fn native_walk_owns_first_visit_admission_and_depth_frontiers() {
        std::env::set_var("MNCS_LIBRARY_PATH", library());
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let expected_dependency_identity = StoreResolver::new(&store)
            .resolve("mncs.std.relation_walk.v1")
            .expect("current Stdlib relation walk module")
            .identity;

        let mut state = execute_walk_start(&cache, &store).expect("native relation walk start");
        assert_eq!(state.frontier, vec![RELATION_WALK_ROOT_CODE]);
        assert_eq!(state.reached_count, 1);
        assert_eq!(
            state.dependency_source_identity,
            expected_dependency_identity
        );
        assert_eq!(state.backend, RESEARCH_BYTECODE_BACKEND_NAME);
        assert!(!state.kernel_source_identity.is_empty());
        assert!(!state.kernel_artifact_identity.is_empty());
        assert!(!state.truncated);

        let first = execute_walk_advance_batch(
            &cache,
            &store,
            &state,
            &[RELATION_WALK_ROOT_CODE, 1, 1, 2],
            1,
            RELATION_WALK_RESULT_CAPACITY,
        )
        .expect("native first depth batch");
        assert_eq!(first.accepted, vec![false, true, false, true]);
        assert_eq!(first.state.frontier, vec![1, 2]);
        assert_eq!(first.state.reached_count, 3);
        state = first.state;

        let second = execute_walk_advance_batch(
            &cache,
            &store,
            &state,
            &[2, 3, 1, 3],
            1,
            RELATION_WALK_RESULT_CAPACITY,
        )
        .expect("native accumulated depth batch");
        assert_eq!(second.accepted, vec![false, true, false, false]);
        assert_eq!(second.state.frontier, vec![1, 2, 3]);
        assert_eq!(second.state.reached_count, 4);
        state = second.state;

        let next_depth = execute_walk_advance_batch(
            &cache,
            &store,
            &state,
            &[],
            2,
            RELATION_WALK_RESULT_CAPACITY,
        )
        .expect("native empty next depth");
        assert!(next_depth.accepted.is_empty());
        assert!(next_depth.state.frontier.is_empty());
        assert_eq!(next_depth.state.reached_count, 4);
        assert_eq!(next_depth.state.frontier_depth, 2);

        let capped_start = execute_walk_start(&cache, &store).expect("native capped walk start");
        let capped = execute_walk_advance_batch(&cache, &store, &capped_start, &[9, 8, 7], 1, 2)
            .expect("native result-budget batch");
        assert_eq!(capped.accepted, vec![true, true, false]);
        assert_eq!(capped.state.frontier, vec![9, 8]);
        assert_eq!(capped.state.reached_count, 3);
        assert!(capped.state.truncated);
    }
}
