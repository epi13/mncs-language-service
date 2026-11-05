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
const RELATION_URI: &str = "mncs://language-service/call-relation-summary.mncs";
const RELATION_SOURCE: &str = include_str!("../../../mncs/call_relation_summary.mncs");
const STEP_BUDGET: u64 = 100_000;

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
    let dependencies = resolver.resolve("mncs.std.sort.v1").ok_or_else(|| {
        "MNCS-native relation summary requires mncs.std.sort.v1 from MNCS_LIBRARY_PATH".to_owned()
    })?;
    if let Ok(read) = cache.read() {
        if let Some(existing) = read.as_ref() {
            if existing.source_identity == envelope.identity
                && existing.dependency_identities.get("mncs.std.sort.v1")
                    == Some(&dependencies.identity)
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
}
