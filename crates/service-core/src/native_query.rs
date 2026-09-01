//! MNCS-native query kernels executed through the authoritative compiler and
//! research-bytecode backend.
//!
//! This module deliberately contains only the boundary adapter: it projects
//! service data into language-owned values, invokes a frozen MNCS artifact,
//! and validates the returned value. Status semantics live in the imported
//! `mncs.core.status.v1` standard-library module.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use mncs_codegen::{execute_backend, RESEARCH_BYTECODE_BACKEND_NAME};
use mncs_compiler::{ModuleResolver, ReferenceCompiler, SourceFrontEndResult};
use mncs_model::{
    ArtifactRepresentation, CompilationStatus, ExecutionRequest, ExecutionStatus, ExecutionTarget,
    ExecutionValue, FiniteType, IntegerType, Program, TransformationStatus,
    EXECUTION_REQUEST_SCHEMA_VERSION,
};

use crate::document::DocumentStore;
use crate::modules::StoreResolver;

pub(crate) const STATUS_MODULE: &str = "mncs.core.status.v1";
pub(crate) const QUERY_MODULE: &str = "mncs.language_service.status_query.v1";
pub(crate) const QUERY_FUNCTION: &str = "summarize_obligations";
pub(crate) const MAX_OBLIGATIONS: usize = 8;
const QUERY_URI: &str = "mncs://language-service/status-query.mncs";
const QUERY_SOURCE: &str = include_str!("../../../mncs/status_query.mncs");
const STEP_BUDGET: u64 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeStatusSummary {
    pub backend: String,
    pub kernel_source_identity: String,
    pub dependency_source_identity: String,
    pub kernel_artifact_identity: String,
    pub input_count: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub unknown_count: usize,
    pub observed_count: usize,
    pub dominant_status: String,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeQueryKernel {
    source_identity: String,
    dependency_identities: BTreeMap<String, String>,
    program: Arc<Program>,
    artifact: Arc<mncs_model::BackendArtifact>,
}

fn status_label(status: &str) -> Result<&'static str, String> {
    match status {
        "pass" => Ok("PASS"),
        "fail" => Ok("FAIL"),
        "unknown" => Ok("UNKNOWN"),
        other => Err(format!(
            "authoritative obligation has unsupported status {other:?}"
        )),
    }
}

fn dependency_identities(resolver: &StoreResolver<'_>) -> Result<BTreeMap<String, String>, String> {
    let envelope = resolver.resolve(STATUS_MODULE).ok_or_else(|| {
        "MNCS-native status query requires MNCS_LIBRARY_PATH to resolve mncs.core.status.v1"
            .to_owned()
    })?;
    Ok(BTreeMap::from([(
        STATUS_MODULE.to_owned(),
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
        "MNCS-native query kernel did not produce a valid authoritative program".to_owned()
    } else {
        format!(
            "MNCS-native query kernel source is unsupported: {}",
            details.join("; ")
        )
    }
}

fn prepare_kernel(
    store: &DocumentStore,
    source_identity: &str,
    dependency_identities: &BTreeMap<String, String>,
) -> Result<NativeQueryKernel, String> {
    let resolver = StoreResolver::new(store);
    let envelope = store.envelope(QUERY_URI, QUERY_SOURCE);
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
                "MNCS-native query kernel request refused: {}",
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
            "MNCS-native query kernel compilation failed: {}",
            details.join("; ")
        ));
    }
    let artifact = compilation
        .emissions
        .backend
        .ok_or_else(|| "MNCS-native query kernel backend artifact was not emitted".to_owned())?;
    if artifact.backend.name != RESEARCH_BYTECODE_BACKEND_NAME
        || !artifact.identity_is_valid()
        || artifact.status != TransformationStatus::Pass
    {
        return Err(
            "MNCS-native query kernel backend artifact failed identity/status validation"
                .to_owned(),
        );
    }
    Ok(NativeQueryKernel {
        source_identity: source_identity.to_owned(),
        dependency_identities: dependency_identities.clone(),
        program,
        artifact: Arc::new(artifact),
    })
}

fn kernel(
    cache: &RwLock<Option<Arc<NativeQueryKernel>>>,
    store: &DocumentStore,
) -> Result<Arc<NativeQueryKernel>, String> {
    let envelope = store.envelope(QUERY_URI, QUERY_SOURCE);
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
        .map_err(|_| "MNCS-native query kernel cache is poisoned".to_owned())?;
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

fn finite_value(finite: &FiniteType, name: &str) -> Result<ExecutionValue, String> {
    let variant = finite
        .variants
        .iter()
        .find(|variant| variant.name == name)
        .ok_or_else(|| format!("status variant {name:?} is not present in the kernel program"))?;
    Ok(ExecutionValue::Finite {
        type_identity: finite.identity.clone(),
        variant_identity: variant.identity.clone(),
        discriminant: variant.discriminant,
        payload: Arc::new(Vec::new()),
    })
}

fn integer_field(value: &ExecutionValue, field: &str) -> Result<usize, String> {
    let ExecutionValue::Integer { value, ty } = value else {
        return Err(format!(
            "native status summary field {field:?} is not an integer"
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
            "native status summary field {field:?} has an invalid integer value"
        ));
    }
    usize::try_from(*value)
        .map_err(|_| format!("native status summary field {field:?} is too large"))
}

fn bool_field(value: &ExecutionValue, field: &str) -> Result<bool, String> {
    let ExecutionValue::Boolean { value } = value else {
        return Err(format!(
            "native status summary field {field:?} is not boolean"
        ));
    };
    Ok(*value)
}

fn status_field(program: &Program, value: &ExecutionValue) -> Result<String, String> {
    let ExecutionValue::Finite {
        type_identity,
        variant_identity,
        ..
    } = value
    else {
        return Err("native status summary status field is not a finite status".to_owned());
    };
    let finite = program
        .finite_types
        .iter()
        .find(|finite| finite.identity == *type_identity && finite.name == "Status")
        .ok_or_else(|| "native status summary returned an unknown Status type".to_owned())?;
    finite
        .variants
        .iter()
        .find(|variant| variant.identity == *variant_identity)
        .map(|variant| variant.name.to_lowercase())
        .ok_or_else(|| "native status summary returned an unknown Status variant".to_owned())
}

fn summary_record(
    program: &Program,
    value: &ExecutionValue,
) -> Result<(String, usize, usize, usize, usize, bool), String> {
    let ExecutionValue::Record {
        type_identity,
        fields,
        ..
    } = value
    else {
        return Err("MNCS-native status query returned a non-record value".to_owned());
    };
    let record = program
        .record_types
        .iter()
        .find(|record| record.identity == *type_identity && record.name == "StatusSummary")
        .ok_or_else(|| "MNCS-native status query returned an unknown summary record".to_owned())?;
    if record.fields.len() != fields.len()
        || record
            .fields
            .iter()
            .zip(fields.iter())
            .any(|(expected, actual)| expected.name != actual.0)
    {
        return Err("MNCS-native status query returned an unexpected summary shape".to_owned());
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("native status summary omitted field {name:?}"))
    };
    let status = status_field(program, field("status")?)?;
    let pass = integer_field(field("pass_count")?, "pass_count")?;
    let fail = integer_field(field("fail_count")?, "fail_count")?;
    let unknown = integer_field(field("unknown_count")?, "unknown_count")?;
    let observed = integer_field(field("observed_count")?, "observed_count")?;
    let valid = bool_field(field("valid")?, "valid")?;
    Ok((status, pass, fail, unknown, observed, valid))
}

pub(crate) fn execute_status_summary(
    cache: &RwLock<Option<Arc<NativeQueryKernel>>>,
    store: &DocumentStore,
    statuses: &[&str],
) -> Result<NativeStatusSummary, String> {
    if statuses.len() > MAX_OBLIGATIONS {
        return Err(format!(
            "MNCS-native status query is bounded to {MAX_OBLIGATIONS} obligations; received {}",
            statuses.len()
        ));
    }
    let kernel = kernel(cache, store)?;
    let finite = kernel
        .program
        .finite_types
        .iter()
        .find(|finite| finite.name == "Status")
        .ok_or_else(|| "MNCS-native query kernel does not expose Status".to_owned())?;
    let mut values = statuses
        .iter()
        .map(|status| status_label(status).and_then(|label| finite_value(finite, label)))
        .collect::<Result<Vec<_>, _>>()?;
    while values.len() < MAX_OBLIGATIONS {
        values.push(finite_value(finite, "UNKNOWN")?);
    }
    let arguments = vec![
        ExecutionValue::Sequence {
            values: Arc::new(values),
        },
        ExecutionValue::Byte {
            value: statuses.len() as i128,
        },
    ];
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: QUERY_MODULE.to_owned(),
            function: QUERY_FUNCTION.to_owned(),
        },
        arguments,
        step_budget: STEP_BUDGET,
        policy: Default::default(),
    };
    let execution = execute_backend(&kernel.artifact, &request);
    if execution.status != ExecutionStatus::Returned || execution.returned.len() != 1 {
        let reason = execution
            .failure
            .map(|failure| failure.reason)
            .unwrap_or_else(|| format!("execution status was {:?}", execution.status));
        return Err(format!(
            "MNCS-native status query execution refused: {reason}"
        ));
    }
    let (dominant_status, pass, fail, unknown, observed, valid) =
        summary_record(&kernel.program, &execution.returned[0])?;
    let dependency_source_identity = kernel
        .dependency_identities
        .get(STATUS_MODULE)
        .cloned()
        .ok_or_else(|| "MNCS-native query kernel lost its status dependency identity".to_owned())?;
    Ok(NativeStatusSummary {
        backend: kernel.artifact.backend.name.clone(),
        kernel_source_identity: kernel.source_identity.clone(),
        dependency_source_identity,
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
        input_count: statuses.len(),
        pass_count: pass,
        fail_count: fail,
        unknown_count: unknown,
        observed_count: observed,
        dominant_status,
        valid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_summary_refuses_inputs_over_the_bounded_envelope() {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let statuses = vec!["pass"; MAX_OBLIGATIONS + 1];

        let error = execute_status_summary(&cache, &store, &statuses).unwrap_err();
        assert!(error.contains("bounded to 8 obligations"));
    }
}
