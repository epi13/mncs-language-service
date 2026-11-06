//! MNCS-native UTF-8 scalar to LSP UTF-16 conversion.
//!
//! MNCS scans long-line chunks into 64-byte checkpoints and materializes a
//! per-byte map for a short window only when a non-ASCII query needs it.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use mncs_codegen::{execute_backend, RESEARCH_BYTECODE_BACKEND_NAME};
use mncs_compiler::{ModuleResolver, ReferenceCompiler, SourceFrontEndResult};
use mncs_model::{
    ArtifactRepresentation, CompilationStatus, ExecutionRequest, ExecutionStatus, ExecutionTarget,
    ExecutionValue, IntegerType, Program, TransformationStatus, EXECUTION_REQUEST_SCHEMA_VERSION,
};

use crate::document::DocumentStore;
use crate::modules::StoreResolver;

pub(crate) const CHUNK_BYTES: usize = 1020;
pub(crate) const CHECKPOINT_BYTES: usize = 64;
pub(crate) const LOOKAHEAD_BYTES: usize = 3;
const POSITION_INPUT_CAPACITY: usize = CHUNK_BYTES + LOOKAHEAD_BYTES;
pub(crate) const WINDOW_INPUT_CAPACITY: usize = CHECKPOINT_BYTES + LOOKAHEAD_BYTES * 2;
const MAP_CAPACITY: usize = CHUNK_BYTES.div_ceil(CHECKPOINT_BYTES) + 1;
const WINDOW_MAP_CAPACITY: usize = WINDOW_INPUT_CAPACITY + 1;
const STEP_BUDGET: u64 = 500_000;

const MODULE: &str = "mncs.language_service.coordinate_map.v1";
const POSITION_FUNCTION: &str = "position_chunk";
const WINDOW_FUNCTION: &str = "coordinate_window";
const SOURCE_URI: &str = "mncs://language-service/coordinate-map.mncs";
const SOURCE: &str = include_str!("../../../mncs/coordinate_map.mncs");
const SCALAR_MODULE: &str = "mncs.std.utf8_scalar.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinateChunk {
    /// Number of bytes safely consumed up to the requested progress limit.
    pub bytes: usize,
    /// Number of UTF-16 code units consumed with those bytes.
    pub units: u64,
    /// Checkpoints packed as `(units * 2048 + snapped byte offset)`.
    pub map: Vec<u32>,
    pub map_length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinateWindow {
    /// Per-byte entries packed as `(UTF-16 units * 128 + snapped byte offset)`.
    pub map: Vec<u16>,
    pub map_length: usize,
}

#[derive(Debug)]
struct Kernel {
    program: Arc<Program>,
    artifact: Arc<mncs_model::BackendArtifact>,
}

static KERNEL: OnceLock<Result<Kernel, String>> = OnceLock::new();

fn frontend_error(front_end: &SourceFrontEndResult) -> String {
    let details = front_end
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{}@{}: {}",
                diagnostic.code, diagnostic.span.start, diagnostic.message
            )
        })
        .collect::<Vec<_>>();
    if details.is_empty() {
        "MNCS coordinate kernel did not produce an authoritative program".to_owned()
    } else {
        format!(
            "MNCS coordinate kernel source is unsupported: {}",
            details.join("; ")
        )
    }
}

fn prepare_kernel() -> Result<Kernel, String> {
    let store = DocumentStore::new(None);
    let resolver = StoreResolver::new(&store);
    if resolver.resolve(SCALAR_MODULE).is_none() {
        return Err(format!(
            "MNCS coordinate kernel requires {SCALAR_MODULE} in the configured library roots"
        ));
    }
    let envelope = store.envelope(SOURCE_URI, SOURCE);
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
                "MNCS coordinate kernel request refused: {}",
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
            "MNCS coordinate kernel compilation failed: {}",
            details.join("; ")
        ));
    }
    let artifact = compilation
        .emissions
        .backend
        .ok_or_else(|| "MNCS coordinate kernel backend artifact was not emitted".to_owned())?;
    if artifact.backend.name != RESEARCH_BYTECODE_BACKEND_NAME
        || !artifact.identity_is_valid()
        || artifact.status != TransformationStatus::Pass
    {
        return Err(
            "MNCS coordinate kernel backend artifact failed identity/status validation".to_owned(),
        );
    }
    Ok(Kernel {
        program,
        artifact: Arc::new(artifact),
    })
}

fn kernel() -> Result<&'static Kernel, String> {
    match KERNEL.get_or_init(prepare_kernel) {
        Ok(kernel) => Ok(kernel),
        Err(error) => Err(error.clone()),
    }
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

fn integer_field(value: &ExecutionValue, name: &str, bits: u16) -> Result<u64, String> {
    let ExecutionValue::Integer { value, ty } = value else {
        return Err(format!(
            "MNCS coordinate result field {name:?} is not an integer"
        ));
    };
    if *ty
        != (IntegerType {
            bits,
            signed: false,
        })
    {
        return Err(format!(
            "MNCS coordinate result field {name:?} has the wrong integer type"
        ));
    }
    u64::try_from(*value)
        .map_err(|_| format!("MNCS coordinate result field {name:?} is out of range"))
}

fn sequence_field<T>(value: &ExecutionValue, name: &str, bits: u16) -> Result<Vec<T>, String>
where
    T: TryFrom<u64>,
{
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!(
            "MNCS coordinate result field {name:?} is not a bounded sequence"
        ));
    };
    values
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            T::try_from(integer_field(entry, &format!("{name}[{index}]"), bits)?)
                .map_err(|_| format!("MNCS coordinate result field {name:?} is out of range"))
        })
        .collect()
}

fn execute(
    function: &str,
    record_name: &str,
    bytes: &[u8],
    numeric_arguments: &[u64],
) -> Result<(Vec<(String, ExecutionValue)>, &'static Kernel), String> {
    let input_limit = match function {
        POSITION_FUNCTION => POSITION_INPUT_CAPACITY,
        WINDOW_FUNCTION => WINDOW_INPUT_CAPACITY,
        _ => return Err(format!("unknown MNCS coordinate function {function:?}")),
    };
    if bytes.len() > input_limit {
        return Err("MNCS coordinate request exceeds its bounded input envelope".to_owned());
    }
    let kernel = kernel()?;
    let values = bytes
        .iter()
        .map(|byte| ExecutionValue::Byte {
            value: i128::from(*byte),
        })
        .collect::<Vec<_>>();
    let mut arguments = vec![ExecutionValue::Sequence {
        values: Arc::new(values),
    }];
    arguments.extend(
        std::iter::once(bytes.len() as u64)
            .chain(numeric_arguments.iter().copied())
            .map(integer_argument),
    );
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: MODULE.to_owned(),
            function: function.to_owned(),
        },
        arguments,
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
        return Err(format!("MNCS coordinate execution refused: {reason}"));
    }
    let ExecutionValue::Record {
        type_identity,
        name,
        fields,
    } = &execution.returned[0]
    else {
        return Err("MNCS coordinate execution returned a non-record value".to_owned());
    };
    let record = kernel
        .program
        .record_types
        .iter()
        .find(|record| record.name == record_name)
        .ok_or_else(|| format!("MNCS coordinate program does not expose {record_name}"))?;
    if type_identity != &record.identity || name != record_name {
        return Err("MNCS coordinate execution returned an unknown record identity".to_owned());
    }
    Ok((fields.as_ref().clone(), kernel))
}

fn record_field<'a>(
    fields: &'a [(String, ExecutionValue)],
    name: &str,
) -> Result<&'a ExecutionValue, String> {
    fields
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("MNCS coordinate result omitted {name:?}"))
}

pub(crate) fn position_chunk(bytes: &[u8], target: usize) -> Result<CoordinateChunk, String> {
    if target > CHUNK_BYTES {
        return Err("MNCS position target exceeds the progress chunk".to_owned());
    }
    let (fields, _) = execute(
        POSITION_FUNCTION,
        "CoordinateChunk",
        bytes,
        &[target as u64],
    )?;
    let consumed = integer_field(record_field(&fields, "bytes")?, "bytes", 64)?;
    let units = integer_field(record_field(&fields, "units")?, "units", 64)?;
    let valid = matches!(
        record_field(&fields, "valid")?,
        ExecutionValue::Boolean { value: true }
    );
    let map_length = integer_field(record_field(&fields, "map_length")?, "map_length", 64)?;
    let map = sequence_field(record_field(&fields, "map")?, "map", 32)?;
    if !valid {
        return Err("MNCS UTF-8 scalar kernel rejected a byte in a Rust UTF-8 string".to_owned());
    }
    let consumed = usize::try_from(consumed)
        .map_err(|_| "MNCS coordinate result byte count is out of range".to_owned())?;
    let map_length = usize::try_from(map_length)
        .map_err(|_| "MNCS coordinate map length is out of range".to_owned())?;
    let expected_map_length = bytes.len().div_ceil(CHECKPOINT_BYTES) + 1;
    if consumed > target
        || consumed > bytes.len()
        || units > bytes.len() as u64
        || map.len() != MAP_CAPACITY
        || map_length != expected_map_length
    {
        return Err("MNCS coordinate result exceeded its input envelope".to_owned());
    }
    Ok(CoordinateChunk {
        bytes: consumed,
        units,
        map,
        map_length,
    })
}

pub(crate) fn coordinate_window(bytes: &[u8]) -> Result<CoordinateWindow, String> {
    let (fields, _) = execute(WINDOW_FUNCTION, "CoordinateWindow", bytes, &[])?;
    let valid = matches!(
        record_field(&fields, "valid")?,
        ExecutionValue::Boolean { value: true }
    );
    let map_length = integer_field(record_field(&fields, "map_length")?, "map_length", 64)?;
    let map = sequence_field(record_field(&fields, "map")?, "map", 16)?;
    if !valid {
        return Err("MNCS UTF-8 scalar kernel rejected a byte in a Rust UTF-8 string".to_owned());
    }
    let map_length = usize::try_from(map_length)
        .map_err(|_| "MNCS coordinate window length is out of range".to_owned())?;
    if map.len() != WINDOW_MAP_CAPACITY || map_length != bytes.len() + 1 {
        return Err("MNCS coordinate window exceeded its input envelope".to_owned());
    }
    Ok(CoordinateWindow { map, map_length })
}

#[cfg(test)]
pub(crate) fn kernel_artifact_identity() -> Result<String, String> {
    Ok(kernel()?.artifact.identity.to_string())
}
