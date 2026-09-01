# MNCS-native language-service conversion record

Status: **implemented / experimental first tranche** (2026-08).

This is the proving-ground record for moving deterministic query logic into
MNCS while keeping the resident service, protocol adapters, and authoritative
semantic acquisition in Rust. The service never creates a second parser,
type system, resolver, obligation model, identity scheme, or semantic ontology.

| Area | Rust | MNCS | Host-bound | Blocked by language | Notes |
| --- | ---: | ---: | ---: | ---: | --- |
| obligation summaries | reference/control, projection, validation, differential policy | bounded `StatusSummary` aggregation through `mncs.core.status.v1::summarize8` | no | no | real `native_obligations` service/MCP path; max 8 inputs |
| symbol queries | authoritative artifact acquisition and current indexes | no | no | no | next conversion candidate: bounded kind/module filtering |
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

## Prioritized next tranche

1. Convert bounded symbol kind/module filtering, retaining authoritative
   declaration identities and deterministic ordering in the Rust control path.
2. Convert a bounded direct relationship summary over existing call edges;
   add explicit duplicate and ordering differential cases.
3. Reuse those bounded projections in candidate reports without duplicating
   `Program::semantic_diff` or invalidation semantics.
4. Pressure general library primitives only when a real query requires them;
   do not introduce unrestricted maps, sets, recursion, or host capabilities
   speculatively.

