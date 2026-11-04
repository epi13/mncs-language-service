//! MNCS-native query kernels for bounded symbol-kind and text-prefix filtering.
//!
//! Kind counting uses `mncs.core.sequences.v1::count`; substring and prefix
//! matching delegate byte scanning to `mncs.std.text_scan.v1`. The service
//! projects bounded inputs, executes the frozen `mncs-research-bytecode`
//! artifact, and validates identity-bound results. Filter semantics live in
//! imported Standard modules.

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
const SYMBOL_MATCH_FUNCTION: &str = "match_names";
const PREFIX_MATCH_FUNCTION: &str = "match_prefixes";
const TEXT_MATCH_RESULT_TYPE: &str = "TextMatchResult";
pub(crate) const MAX_FILTER_TAGS: usize = 8;
pub(crate) const MAX_TEXT_MATCH_ITEMS: usize = 64;
pub(crate) const MAX_TEXT_MATCH_BYTES: usize = 64;
// `text_scan.contains` examines at most haystack_length * 64 byte slots.
// Keep one call to eight full-width windows including per-start overhead, even
// though the fixed result envelope can select up to 64 shorter names.
pub(crate) const MAX_SYMBOL_SCAN_WORK_PER_CALL: usize =
    (MAX_TEXT_MATCH_ITEMS / 8) * (MAX_TEXT_MATCH_BYTES + 1) * MAX_TEXT_MATCH_BYTES;
const MAX_PREFIX_SCAN_WORK_PER_CALL: usize = MAX_TEXT_MATCH_ITEMS * MAX_TEXT_MATCH_BYTES;
const TEXT_SCAN_MODULE: &str = "mncs.std.text_scan.v1";
const BYTES_MODULE: &str = "mncs.core.bytes.v1";
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
    pub dependency_source_identities: BTreeMap<String, String>,
    pub kernel_artifact_identity: String,
    pub input_tags: Vec<i64>,
    pub wanted_tag: i64,
    pub reference_count: usize,
    pub native_count: usize,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeTextMatchSummary {
    pub backend: String,
    pub kernel_source_identity: String,
    pub dependency_source_identity: String,
    pub dependency_source_identities: BTreeMap<String, String>,
    pub kernel_artifact_identity: String,
    pub input_count: usize,
    pub matched: Vec<bool>,
    pub matched_count: usize,
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
    [FILTER_MODULE, TEXT_SCAN_MODULE, BYTES_MODULE]
        .into_iter()
        .map(|module| {
            let envelope = resolver.resolve(module).ok_or_else(|| {
                format!("MNCS-native filter query requires MNCS_LIBRARY_PATH to resolve {module}")
            })?;
            Ok((module.to_owned(), envelope.identity))
        })
        .collect()
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
        dependency_source_identities: kernel.dependency_identities.clone(),
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
        input_tags: full_tags,
        wanted_tag: wanted,
        reference_count,
        native_count: usize::try_from(native_count.max(0))
            .map_err(|_| "MNCS-native filter query returned a negative count".to_owned())?,
        valid: true,
    })
}

fn byte_view_argument(bytes: &[u8]) -> ExecutionValue {
    ExecutionValue::Sequence {
        values: Arc::new(
            bytes
                .iter()
                .map(|value| ExecutionValue::Byte {
                    value: i128::from(*value),
                })
                .collect(),
        ),
    }
}

fn fixed_byte_argument(bytes: &[u8], width: usize) -> ExecutionValue {
    let mut values = bytes
        .iter()
        .map(|value| ExecutionValue::Byte {
            value: i128::from(*value),
        })
        .collect::<Vec<_>>();
    while values.len() < width {
        values.push(ExecutionValue::Byte { value: 0 });
    }
    ExecutionValue::Sequence {
        values: Arc::new(values),
    }
}

fn boolean_sequence_result(value: &ExecutionValue, label: &str) -> Result<Vec<bool>, String> {
    let ExecutionValue::Sequence { values } = value else {
        return Err(format!("MNCS-native {label} returned a non-sequence mask"));
    };
    if values.len() != MAX_TEXT_MATCH_ITEMS {
        return Err(format!(
            "MNCS-native text match returned {} mask slots instead of {}",
            values.len(),
            MAX_TEXT_MATCH_ITEMS
        ));
    }
    values
        .iter()
        .map(|value| match value {
            ExecutionValue::Boolean { value } => Ok(*value),
            _ => Err("MNCS-native text match returned a non-boolean mask slot".to_owned()),
        })
        .collect()
}

/// Match a bounded batch of already-normalized UTF-8 names in MNCS.
/// Lowercasing remains a host text-boundary operation until MNCS has an
/// authoritative Unicode lowercase API. Inputs are rejected rather than
/// truncated so callers can preserve compatibility for larger names.
pub(crate) fn execute_symbol_matches(
    cache: &RwLock<Option<Arc<NativeFilterKernel>>>,
    store: &DocumentStore,
    names: &[Vec<u8>],
    needle: &[u8],
) -> Result<NativeTextMatchSummary, String> {
    execute_text_matches(cache, store, TextMatchKind::Contains, names, needle)
}

/// Match a bounded batch of UTF-8 completion labels against one exact prefix.
/// For valid UTF-8, byte-prefix equality has the same result as `str::starts_with`.
pub(crate) fn execute_prefix_matches(
    cache: &RwLock<Option<Arc<NativeFilterKernel>>>,
    store: &DocumentStore,
    names: &[Vec<u8>],
    prefix: &[u8],
) -> Result<NativeTextMatchSummary, String> {
    execute_text_matches(cache, store, TextMatchKind::Prefix, names, prefix)
}

#[derive(Clone, Copy)]
enum TextMatchKind {
    Contains,
    Prefix,
}

impl TextMatchKind {
    fn function(self) -> &'static str {
        match self {
            Self::Contains => SYMBOL_MATCH_FUNCTION,
            Self::Prefix => PREFIX_MATCH_FUNCTION,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Contains => "symbol matching",
            Self::Prefix => "completion prefix matching",
        }
    }

    fn max_scan_work(self) -> usize {
        match self {
            Self::Contains => MAX_SYMBOL_SCAN_WORK_PER_CALL,
            Self::Prefix => MAX_PREFIX_SCAN_WORK_PER_CALL,
        }
    }

    fn scan_work(self, name: &[u8]) -> usize {
        match self {
            Self::Contains => name
                .len()
                .saturating_add(1)
                .saturating_mul(MAX_TEXT_MATCH_BYTES),
            // `text_scan.starts_with<64>` checks at most 64 needle slots.
            Self::Prefix => MAX_TEXT_MATCH_BYTES,
        }
    }
}

fn execute_text_matches(
    cache: &RwLock<Option<Arc<NativeFilterKernel>>>,
    store: &DocumentStore,
    kind: TextMatchKind,
    names: &[Vec<u8>],
    needle: &[u8],
) -> Result<NativeTextMatchSummary, String> {
    let label = kind.label();
    if names.len() > MAX_TEXT_MATCH_ITEMS {
        return Err(format!(
            "MNCS-native {label} is bounded to {MAX_TEXT_MATCH_ITEMS} names; received {}",
            names.len()
        ));
    }
    if needle.len() > MAX_TEXT_MATCH_BYTES {
        return Err(format!(
            "MNCS-native {label} is bounded to {MAX_TEXT_MATCH_BYTES} needle bytes; received {}",
            needle.len()
        ));
    }
    if let Some((index, name)) = names
        .iter()
        .enumerate()
        .find(|(_, name)| name.len() > MAX_TEXT_MATCH_BYTES)
    {
        return Err(format!(
            "MNCS-native {label} name {index} exceeds {MAX_TEXT_MATCH_BYTES} bytes: {}",
            name.len()
        ));
    }
    let scan_work = names
        .iter()
        .map(|name| kind.scan_work(name))
        .fold(0usize, usize::saturating_add);
    let max_scan_work = kind.max_scan_work();
    if scan_work > max_scan_work {
        return Err(format!(
            "MNCS-native {label} exceeds the per-call scan bound: {scan_work} > {max_scan_work}"
        ));
    }
    let kernel = kernel(cache, store)?;
    let values: Vec<ExecutionValue> = names.iter().map(|name| byte_view_argument(name)).collect();
    let request = ExecutionRequest {
        schema_version: EXECUTION_REQUEST_SCHEMA_VERSION.to_owned(),
        target: ExecutionTarget {
            module: FILTER_QUERY_MODULE.to_owned(),
            function: kind.function().to_owned(),
        },
        arguments: vec![
            ExecutionValue::Sequence {
                values: Arc::new(values),
            },
            fixed_byte_argument(needle, MAX_TEXT_MATCH_BYTES),
            ExecutionValue::Byte {
                value: needle.len() as i128,
            },
        ],
        type_arguments: Vec::new(),
        step_budget: 1_000_000,
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
        return Err(format!("MNCS-native {label} execution refused: {reason}"));
    }
    let ExecutionValue::Record {
        type_identity,
        name: result_name,
        fields,
    } = &execution.returned[0]
    else {
        return Err(format!("MNCS-native {label} returned a non-record value"));
    };
    let record = kernel
        .program
        .record_types
        .iter()
        .find(|record| &record.identity == type_identity && record.name == TEXT_MATCH_RESULT_TYPE)
        .ok_or_else(|| format!("MNCS-native {label} returned an unknown record"))?;
    if result_name != TEXT_MATCH_RESULT_TYPE
        || record.fields.len() != fields.len()
        || record
            .fields
            .iter()
            .zip(fields.iter())
            .any(|(expected, actual)| expected.name != actual.0)
    {
        return Err(format!(
            "MNCS-native {label} returned an unexpected record shape"
        ));
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("MNCS-native {label} omitted field {name:?}"))
    };
    let matched = boolean_sequence_result(field("matched")?, label)?;
    let count = integer_result(&kernel.program, field("matched_count")?)?;
    let valid = match field("valid")? {
        ExecutionValue::Boolean { value } => *value,
        _ => {
            return Err(format!(
                "MNCS-native {label} returned a non-boolean validity flag"
            ))
        }
    };
    let matched_count = usize::try_from(count)
        .map_err(|_| format!("MNCS-native {label} returned an invalid match count"))?;
    let mask_count = matched.iter().filter(|value| **value).count();
    if !valid || matched_count != mask_count || matched_count > names.len() {
        return Err(format!("MNCS-native {label} failed result validation"));
    }
    let dependency_source_identity = kernel
        .dependency_identities
        .get(TEXT_SCAN_MODULE)
        .cloned()
        .ok_or_else(|| format!("MNCS-native {label} lost its text-scan dependency identity"))?;
    Ok(NativeTextMatchSummary {
        backend: kernel.artifact.backend.name.clone(),
        kernel_source_identity: kernel.source_identity.clone(),
        dependency_source_identity,
        dependency_source_identities: kernel.dependency_identities.clone(),
        kernel_artifact_identity: kernel.artifact.identity.0.clone(),
        input_count: names.len(),
        matched,
        matched_count,
        valid,
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

    #[test]
    fn native_workspace_name_match_handles_substrings_and_utf8_bytes() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let library = [
            manifest_dir.join("../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../../mncs-stdlib/library"),
        ]
        .into_iter()
        .find(|path| path.join("core/sequences.mncs").is_file())
        .expect("sibling MNCS standard library");
        assert!(library.join("core/sequences.mncs").is_file());
        std::env::set_var("MNCS_LIBRARY_PATH", &library);

        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let names = vec![
            b"remotething".to_vec(),
            b"motor".to_vec(),
            b"unrelated".to_vec(),
        ];
        let first = execute_symbol_matches(&cache, &store, &names, b"mot")
            .expect("bounded MNCS name match");
        assert_eq!(first.matched[..3], [true, true, false]);
        assert!(first.matched[3..].iter().all(|matched| !matched));
        assert_eq!(first.matched_count, 2);
        assert!(first.valid);
        assert_eq!(first.backend, RESEARCH_BYTECODE_BACKEND_NAME);
        assert_eq!(first.input_count, names.len());
        assert!(!first.kernel_source_identity.is_empty());
        assert_eq!(first.dependency_source_identities.len(), 3);
        assert_eq!(
            first.dependency_source_identity,
            first.dependency_source_identities[TEXT_SCAN_MODULE]
        );

        let unicode = execute_symbol_matches(
            &cache,
            &store,
            &["caf\u{00e9}".as_bytes().to_vec()],
            "\u{00e9}".as_bytes(),
        )
        .expect("UTF-8 byte substring match");
        assert_eq!(unicode.matched[0], true);
        assert_eq!(unicode.matched_count, 1);
        assert_eq!(
            first.dependency_source_identities,
            unicode.dependency_source_identities
        );
        assert_eq!(
            first.kernel_artifact_identity,
            unicode.kernel_artifact_identity
        );

        let rejected_names = vec![vec![b'a'; MAX_TEXT_MATCH_BYTES]; MAX_TEXT_MATCH_ITEMS];
        let mut long_needle = vec![b'a'; MAX_TEXT_MATCH_BYTES - 1];
        long_needle.push(b'b');
        let rejected = execute_symbol_matches(&cache, &store, &rejected_names, &long_needle)
            .expect_err("over-budget work envelope is refused before execution");
        assert!(rejected.contains("per-call scan bound"));

        let bounded_worst_case_names =
            vec![
                vec![b'a'; MAX_TEXT_MATCH_BYTES];
                MAX_SYMBOL_SCAN_WORK_PER_CALL / ((MAX_TEXT_MATCH_BYTES + 1) * MAX_TEXT_MATCH_BYTES)
            ];
        let bounded_worst_case =
            execute_symbol_matches(&cache, &store, &bounded_worst_case_names, &long_needle)
                .expect("maximum admitted name-match envelope");
        assert_eq!(
            bounded_worst_case.input_count,
            bounded_worst_case_names.len()
        );
        assert_eq!(bounded_worst_case.matched_count, 0);
        assert!(bounded_worst_case.valid);
    }

    #[test]
    fn native_workspace_name_match_refuses_oversized_values_without_truncation() {
        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let name = vec![b'x'; MAX_TEXT_MATCH_BYTES + 1];
        let error = execute_symbol_matches(&cache, &store, &[name], b"x").unwrap_err();
        assert!(error.contains("exceeds 64 bytes"));
    }

    #[test]
    fn native_completion_prefix_matches_match_rust_for_utf8_and_empty_prefix() {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let library = [
            manifest_dir.join("../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../mncs-stdlib/library"),
            manifest_dir.join("../../../../../mncs-stdlib/library"),
        ]
        .into_iter()
        .find(|path| path.join("core/sequences.mncs").is_file())
        .expect("sibling MNCS standard library");
        std::env::set_var("MNCS_LIBRARY_PATH", &library);

        let cache = RwLock::new(None);
        let store = DocumentStore::new(None);
        let names = ["return", "record", "café", "cafeteria", "λambda", "x"]
            .map(|name| name.as_bytes().to_vec());
        for prefix in ["ret", "caf", "café", "λ", "missing", ""] {
            let expected: Vec<bool> = names
                .iter()
                .map(|name| {
                    std::str::from_utf8(name)
                        .expect("test names are UTF-8")
                        .starts_with(prefix)
                })
                .collect();
            let native = execute_prefix_matches(&cache, &store, &names, prefix.as_bytes())
                .expect("bounded MNCS completion prefix match");
            assert_eq!(native.matched.get(..names.len()), Some(expected.as_slice()));
            assert_eq!(
                native.matched_count,
                expected.iter().filter(|matched| **matched).count()
            );
            assert!(native.valid);
            assert_eq!(native.backend, RESEARCH_BYTECODE_BACKEND_NAME);
            assert_eq!(native.input_count, names.len());
        }

        let oversized = vec![b'x'; MAX_TEXT_MATCH_BYTES + 1];
        assert!(execute_prefix_matches(&cache, &store, &[oversized], b"x")
            .expect_err("oversized labels are never truncated")
            .contains("exceeds 64 bytes"));
    }
}
