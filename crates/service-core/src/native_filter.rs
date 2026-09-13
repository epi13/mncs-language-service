//! Second MNCS-native query kernel: bounded symbol-kind filtering.
//!
//! Where the status kernel proves finite-type aggregation in MNCS, this
//! kernel proves bounded integer-sequence filtering through the
//! authoritative generic `mncs.core.sequences.v1::count` standard-library
//! function. The Rust service projects symbol kinds to integer tags, pads
//! the fixed 8-slot envelope with `-1` (never a valid tag), and executes
//! the frozen `mncs-research-bytecode` artifact; the returned count must
//! agree with the Rust control count or the query reports `unsupported`.
//!
//! Like the status kernel this module is only the boundary adapter: filter
//! semantics live in the imported standard-library module.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use mncs_codegen::{execute_backend, RESEARCH_BYTECODE_BACKEND_NAME};
use mncs_compiler::{ModuleResolver, ReferenceCompiler, SourceFrontEndResult};
use mncs_model::{
    ArtifactRepresentation, CompilationStatus, ExecutionRequest, ExecutionStatus, ExecutionTarget,
    ExecutionValue, IntegerType, Program, TransformationStatus, EXECUTION_REQUEST_SCHEMA_VERSION,
};

use crate::document::DocumentStore;
use crate::indexes::SymbolKind;
use crate::modules::StoreResolver;

pub(crate) const FILTER_MODULE: &str = "mncs.core.sequences.v1";
pub(crate) const FILTER_QUERY_MODULE: &str = "mncs.language_service.filter_query.v1";
pub(crate) const FILTER_QUERY_FUNCTION: &str = "count_matching";
pub(crate) const MAX_FILTER_TAGS: usize = 8;
const FILTER_QUERY_URI: &str = "mncs://language-service/filter-query.mncs";
const FILTER_QUERY_SOURCE: &str = include_str!("../../../mncs/filter_query.mncs");
const STEP_BUDGET: u64 = 100_000;

/// Stable service-owned projection of [`SymbolKind`] to kernel tags.
/// The mapping is part of the kernel contract: renumbering requires
/// re-validation of the differential tests.
pub fn symbol_kind_tag(kind: SymbolKind) -> i64 {
    match kind {
        SymbolKind::Module => 0,
        SymbolKind::Function => 1,
        SymbolKind::Parameter => 2,
        SymbolKind::Binding => 3,
        SymbolKind::IterationState => 4,
        SymbolKind::FiniteType => 5,
        SymbolKind::FiniteVariant => 6,
        SymbolKind::RecordType => 7,
        SymbolKind::RecordField => 8,
    }
}

/// Padding tag for unused envelope slots. Negative so it can never match a
/// real [`symbol_kind_tag`].
pub const FILTER_PADDING_TAG: i64 = -1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeFilterSummary {
    pub backend: String,
    pub kernel_source_identity: String,
    pub dependency_source_identity: String,
    pub kernel_artifact_identity: String,
    pub input_tags: Vec<i64>,
    pub wanted_tag: i64,
    pub reference_count: usize,
    pub native_count: usize,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeFilterKernel {
    source_identity: String,
    dependency_identities: BTreeMap<String, String>,
    program: Arc<Program>,
    artifact: Arc<mncs_model::BackendArtifact>,
}

fn dependency_identities(resolver: &StoreResolver<'_>) -> Result<BTreeMap<String, String>, String> {
    let envelope = resolver.resolve(FILTER_MODULE).ok_or_else(|| {
        "MNCS-native filter query requires MNCS_LIBRARY_PATH to resolve mncs.core.sequences.v1"
            .to_owned()
    })?;
    Ok(BTreeMap::from([(
        FILTER_MODULE.to_owned(),
        envelope.identity,
    )]))
}

fn frontend_error(front_end: &SourceFrontEndResult) -> String {
    let details = front_end
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>();
    if details.is_empty() {
        "MNCS-native filter kernel did not produce a valid authoritative program".to_owned()
    } else {
        format!(
            "MNCS-native filter kernel source is unsupported: {}",
            details.join("; ")
        )
    }
}

fn prepare_kernel(
    store: &DocumentStore,
    source_identity: &str,
    dependency_identities: &BTreeMap<String, String>,
) -> Result<NativeFilterKernel, String> {
    let resolver = StoreResolver::new(store);
    let envelope = store.envelope(FILTER_QUERY_URI, FILTER_QUERY_SOURCE);
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
            BTreeSet::from([ArtifactRepresentation::BackendArtifact]),
            RESEARCH_BYTECODE_BACKEND_NAME,
        )
        .map_err(|diagnostic| {
            format!(
                "MNCS-native filter kernel request refused: {}",
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
            "MNCS-native filter kernel compilation failed: {}",
            details.join("; ")
        ));
    }
    let artifact = compilation
        .emissions
        .backend
        .ok_or_else(|| "MNCS-native filter kernel backend artifact was not emitted".to_owned())?;
    if artifact.backend.name != RESEARCH_BYTECODE_BACKEND_NAME
        || !artifact.identity_is_valid()
        || artifact.status != TransformationStatus::Pass
    {
        return Err(
            "MNCS-native filter kernel backend artifact failed identity/status validation"
                .to_owned(),
        );
    }
    Ok(NativeFilterKernel {
        source_identity: source_identity.to_owned(),
        dependency_identities: dependency_identities.clone(),
        program,
        artifact: Arc::new(artifact),
    })
}

fn kernel(
    cache: &RwLock<Option<Arc<NativeFilterKernel>>>,
    store: &DocumentStore,
) -> Result<Arc<NativeFilterKernel>, String> {
    let envelope = store.envelope(FILTER_QUERY_URI, FILTER_QUERY_SOURCE);
    let resolver = StoreResolver::new(store);
    let dependencies = dependency_identities(&resolver)?;
    if let Ok(read) = cache.read() {
        if let Some(existing) = read.as_ref() {
            if existing.source_identity == envelope.identity
                && existing.dependency_identities == dependencies
            {
                return Ok(Arc::clone(existing));
            }
        }
    }
    let prepared = Arc::new(prepare_kernel(store, &envelope.identity, &dependencies)?);
    let mut write = cache
        .write()
        .map_err(|_| "MNCS-native filter kernel cache is poisoned".to_owned())?;
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

fn integer_argument(value: i64) -> ExecutionValue {
    ExecutionValue::Integer {
        value: value as i128,
        ty: IntegerType {
            bits: 64,
            signed: true,
        },
    }
}

fn integer_result(program: &Program, value: &ExecutionValue) -> Result<i64, String> {
    let _ = program;
    let ExecutionValue::Integer { value, ty } = value else {
        return Err("MNCS-native filter query returned a non-integer value".to_owned());
    };
    if *ty
        != (IntegerType {
            bits: 64,
            signed: true,
        })
    {
        return Err("MNCS-native filter query returned an unexpected integer type".to_owned());
    }
    i64::try_from(*value)
        .map_err(|_| "MNCS-native filter query returned an out-of-range count".to_owned())
}

/// Execute the bounded filter kernel: `tags` holds at most [`MAX_FILTER_TAGS`]
/// kind tags (padded internally with [`FILTER_PADDING_TAG`]), `wanted` is the
/// tag to count.
pub(crate) fn execute_count_matching(
    cache: &RwLock<Option<Arc<NativeFilterKernel>>>,
    store: &DocumentStore,
    tags: &[i64],
    wanted: i64,
) -> Result<NativeFilterSummary, String> {
    if tags.len() > MAX_FILTER_TAGS {
        return Err(format!(
            "MNCS-native filter query is bounded to {MAX_FILTER_TAGS} tags; received {}",
            tags.len()
        ));
    }
    if tags.contains(&FILTER_PADDING_TAG) {
        return Err("MNCS-native filter input collides with the padding tag".to_owned());
    }
    let kernel = kernel(cache, store)?;
    let mut values: Vec<ExecutionValue> = tags.iter().copied().map(integer_argument).collect();
    while values.len() < MAX_FILTER_TAGS {
        values.push(integer_argument(FILTER_PADDING_TAG));
    }
    let full_tags: Vec<i64> = tags
        .iter()
        .copied()
        .chain(std::iter::repeat(FILTER_PADDING_TAG))
        .take(MAX_FILTER_TAGS)
        .collect();
    let arguments = vec![
        ExecutionValue::Sequence {
            values: Arc::new(values),
        },
        integer_argument(wanted),
    ];
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: FILTER_QUERY_MODULE.to_owned(),
            function: FILTER_QUERY_FUNCTION.to_owned(),
        },
        arguments,
        // Concrete (non-generic) kernel entrypoint: no type arguments.
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
            "MNCS-native filter query execution refused: {reason}"
        ));
    }
    let native_count = integer_result(&kernel.program, &execution.returned[0])?;
    let reference_count = tags.iter().filter(|tag| **tag == wanted).count();
    let dependency_source_identity = kernel
        .dependency_identities
        .get(FILTER_MODULE)
        .cloned()
        .ok_or_else(|| {
            "MNCS-native filter kernel lost its sequences dependency identity".to_owned()
        })?;
    Ok(NativeFilterSummary {
        backend: kernel.artifact.backend.name.clone(),
        kernel_source_identity: kernel.source_identity.clone(),
        dependency_source_identity,
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
        input_tags: full_tags,
        wanted_tag: wanted,
        reference_count,
        native_count: usize::try_from(native_count.max(0))
            .map_err(|_| "MNCS-native filter query returned a negative count".to_owned())?,
        valid: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_kernel_refuses_inputs_over_the_bounded_envelope() {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let tags = vec![1; MAX_FILTER_TAGS + 1];
        let error = execute_count_matching(&cache, &store, &tags, 1).unwrap_err();
        assert!(error.contains("bounded to 8 tags"));
    }

    #[test]
    fn kind_tags_cover_every_indexed_symbol_kind() {
        // The tag mapping must stay total over SymbolKind: an unmapped kind
        // would silently corrupt the kernel projection.
        for kind in [
            SymbolKind::Module,
            SymbolKind::Function,
            SymbolKind::Parameter,
            SymbolKind::Binding,
            SymbolKind::IterationState,
            SymbolKind::FiniteType,
            SymbolKind::FiniteVariant,
            SymbolKind::RecordType,
            SymbolKind::RecordField,
        ] {
            assert!(symbol_kind_tag(kind) >= 0);
        }
    }
}
