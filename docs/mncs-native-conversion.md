# MNCS-native language-service conversion record

Status: **implemented / experimental first tranche** (updated 2026-11).

This is the proving-ground record for moving deterministic query logic into
MNCS while keeping the resident service, protocol adapters, and authoritative
semantic acquisition in Rust. The service never creates a second parser,
type system, resolver, obligation model, identity scheme, or semantic ontology.

| Area | Rust | MNCS | Host-bound | Blocked by language | Notes |
| --- | ---: | ---: | ---: | ---: | --- |
| obligation summaries | reference/control, projection, validation, differential policy | bounded `StatusSummary` aggregation through `mncs.core.status.v1::summarize8` | no | no | real `native_obligations` service/MCP path; max 8 inputs |
| symbol queries | authoritative artifact acquisition, current indexes, Unicode lowercase normalization, oversized-input fallback, stable ordering | bounded case-insensitive byte substring matching through `mncs.std.text_scan.v1` and `mncs.core.sequences.v1::count_true<64>` | yes: normalization and compatibility fallback | no | `workspace_symbols` executes up to 64 names per call; each name is up to 64 UTF-8 bytes |
| relation queries | authoritative call-edge extraction and ordering | no | no | no | next candidate: bounded deterministic edge summaries |
| candidate summaries | language-owned `Program::semantic_diff` and invalidation | no | no | no | do not shadow authoritative semantic diff |
| state transitions | document lifecycle, snapshots, locks, invalidation orchestration | no | yes | no | host/service state remains Rust in this tranche |
| LSP transport | adapter and lifecycle | no | yes | no | unchanged |
| MCP transport | adapter and lifecycle | no | yes | no | `native_obligations` is a thin read-only adapter |

## First vertical slice

For an answered obligation query, `mncs-language-service` obtains the exact
authoritative `ObligationRecord` set, projects only its status labels into a
fixed eight-element `Status` sequence plus a count, executes
`mncs/status_query.mncs` through `ReferenceCompiler` and the real
`mncs-research-bytecode` backend, then validates the identity-bound
`StatusSummary` record. The response includes the Rust reference counts, the
MNCS counts, source/dependency/artifact identities, and unresolved reasons.

The response is `unsupported` if the standard-library dependency is missing,
the query source cannot elaborate or compile, the backend does not return the
expected record, the bound is exceeded, or any differential check fails.
`UNKNOWN` remains a status value inside the bounded lattice; it is not changed
to `PASS` because execution succeeded.

## Evidence and limitations

- The service integration test covers mixed PASS/UNKNOWN obligations,
  differential equality, nominal return validation, and frozen-artifact reuse.
- The service unit test refuses an input over the eight-obligation bound.
- The upstream compiler tests cover imported bounded sequence identity
  preservation and fail-closed unknown sequence elements. That general
  module-linking pressure is recorded in
  `mncs-language/docs/development-evidence/mncs-native-service-query-2026-08.md`.
- This is bounded corpus agreement, not proof of compiler or backend
  equivalence. Only research-bytecode support is claimed for this query.
- The service still requires `MNCS_LIBRARY_PATH` to resolve the authoritative
  status module; it does not copy that module into the service.

## Workspace symbol substring slice

`LanguageService::workspace_symbols` projects already-lowercased indexed
names and the lowercased query into the bounded `match_names` entrypoint in
`mncs/filter_query.mncs`. MNCS compares up to 64 UTF-8 byte views per call,
and delegates literal byte scanning to `mncs.std.text_scan.v1`; it uses the
shared `count_true<64>` operation to summarize its result mask. The adapter
caps estimated scan work at 33,280 byte slots per execution and forms smaller
batches for longer names, keeping the bytecode step budget effective across
the full admitted input width.
The adapter validates the result record, mask width, match count, backend,
and source/dependency-bound cached artifact before mapping selected indexes
back to authoritative symbol summaries. The MCP and LSP adapters both use the
same resident service query.

Rust still applies `String::to_lowercase()` because MNCS does not yet provide
equivalent full Unicode lowercase semantics. Names or queries over 64 bytes
use the prior host substring path; missing Standard resolution or backend
execution also uses that compatibility path. Inputs are never truncated.
Empty queries keep the existing all-symbol behavior without invoking the
kernel. This transfers bounded substring selection to MNCS while leaving the
Unicode text boundary explicit.

The service-core native-kernel test checks interior substrings, a multibyte
UTF-8 substring, result-mask/count agreement, and artifact reuse. An
integration test exercises case-insensitive interior matching through the
actual workspace-symbol consumer. Artifact provenance records the sequences,
byte, and text-scan module identities. This is bounded research-bytecode
evidence, not a claim that every text-related host responsibility has moved.

## Prioritized next tranche

1. Convert a bounded direct relationship summary over existing call edges;
   add explicit duplicate and ordering differential cases.
2. Reuse those bounded projections in candidate reports without duplicating
   `Program::semantic_diff` or invalidation semantics.
3. Pressure general library primitives only when a real query requires them;
   do not introduce unrestricted maps, sets, recursion, or host capabilities
   speculatively.

