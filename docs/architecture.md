# Architecture

## Purpose

The MNCS Language Service is a persistent semantic observation and interaction layer over the authoritative MNCS language implementation.

Its purpose is to keep language understanding resident, queryable, identity-aware, and reusable across editors, coding agents, and MNCS ecosystem services without duplicating compiler or language semantics.

## Architectural contract

The central rule is:

> The MNCS Language Service is a semantic observer and interaction layer, not a second implementation of the MNCS language.

If the service needs a semantic fact that `mncs-language` cannot currently provide, the preferred fix is to expose or implement that capability in `mncs-language`, then consume it here.

This repository may cache, index, project, explain, route, and bind semantic information to workspace snapshots. It must not silently reinterpret language meaning.

## Logical layers

The service is expected to evolve around four conceptual layers. These are architectural responsibilities, not frozen crate names.

### 1. Authoritative language layer

Owned externally by `mncs-language`.

Responsibilities include:

- source envelopes and source profiles;
- lexical, CST, AST, and semantic structures;
- canonical identities;
- validation and diagnostics;
- semantic graphs;
- obligations, evidence, and verification semantics;
- compiler stages, IR, lowering, and backend contracts.

### 2. Resident service core

Owned here.

Responsibilities should include:

- workspace lifecycle;
- open and on-disk document state;
- versioned analysis snapshots;
- reuse and invalidation of analysis results;
- source position mapping;
- symbol, identity, reference, and dependency indexes;
- query execution against a known snapshot;
- cancellation and bounded resource use;
- service health and observability.

### 3. Service query / protocol model

A protocol-neutral semantic interaction model between the resident core and protocol adapters.

This layer should prevent LSP, MCP, or another transport from becoming the service's internal ontology.

Requests and responses should be able to carry exact identities, snapshot references, evidence state, unresolved status, and semantic relationships where those concepts exist in the language.

The exact schema is deliberately not frozen yet.

### 4. Protocol adapters

Adapters translate role-specific protocols into the shared service query model.

Expected adapters include:

- LSP for editor and IDE compatibility;
- MCP for general agent/tool interoperability;
- a future MNCS-native interface for richer ecosystem coordination.

Adapters may project or simplify information for their clients, but must not strengthen semantic claims.

## Dependency direction

The intended dependency direction is one-way:

```text
mncs-language
      ↑
      │ authoritative APIs
      │
service core
      ↑
      │
service query model
      ↑
      │
LSP / MCP / MNCS-native adapters
```

`mncs-language` must remain independently usable without this repository.

## Resident workspace model

A primary reason for this service to exist separately from the compiler CLI is persistence.

The service should eventually retain enough workspace knowledge to answer repeated semantic questions without forcing every client or agent to rediscover the program from raw text.

Conceptually, a workspace may evolve through identity-bound snapshots:

```text
workspace state
      │
      ├── source/document versions
      ├── language frontend outputs
      ├── semantic identities
      ├── indexes and dependency relations
      ├── diagnostics
      ├── obligations/evidence state
      └── derived query caches
```

Snapshot identity and invalidation policy should derive from authoritative language identities wherever practical rather than introducing an unrelated notion of semantic identity.

## Incrementality

Incremental behavior is a service concern; semantic correctness is still a language concern.

The service may cache or selectively recompute stages, but the result for a snapshot must be observationally consistent with invoking the authoritative language APIs for that same source state.

Early implementation should prefer correct coarse invalidation over clever incremental logic. Fine-grained invalidation can be introduced as the language APIs stabilize and evidence supports it.

## Interaction classes

The service should distinguish at least three broad interaction classes.

### Observation

Read-only semantic queries such as identity lookup, description, navigation, dependencies, diagnostics, obligations, evidence, and impact inspection.

These are the safest first capabilities to implement.

### Candidate analysis

Evaluate a proposed source or semantic candidate without promoting it to the workspace baseline.

Candidate state should remain distinguishable from trusted/current workspace state.

### Mutation

Refactoring, semantic patches, or other edits that alter source/workspace state.

Mutation should be introduced only after snapshot identity, stale-state handling, and candidate validation are robust enough to fail closed rather than apply ambiguous changes.

## Analysis tiers

The service should not treat all analysis as equally cheap or equally authoritative.

A future implementation should make analysis tiers explicit, for example:

```text
edit-time
  syntax / parsing / basic semantic analysis

incremental semantic
  identity / references / dependencies / obligations

explicit local verification
  bounded deterministic checks requested by a client

explicit external work
  Forge search / backend execution / Fabric work / independent verification
```

Expensive or externally effectful work must not be triggered merely because a user typed a character or requested hover information.

## State and evidence

The service should preserve the language's distinctions between facts, claims, evidence, assumptions, requirements, preferences, and unresolved obligations.

In particular:

- `UNKNOWN` remains distinct from success;
- absence of evidence does not become evidence of absence;
- cached evidence must retain freshness and subject identity;
- bounded observations must not be presented as universal proofs;
- protocol adapters may simplify presentation, but not meaning.

## Family integration

The service should integrate with the broader MNCS family through explicit boundaries.

### Forge

Forge may use the service to locate semantic subjects, gather candidate context, and request language-owned obligations or facts. Forge remains a search/development harness rather than semantic authority.

### RAVEL

RAVEL may use the service as a resident semantic window into a workspace, allowing tasks and handoffs to refer to semantic identities and snapshots instead of relying only on raw repository text.

### Fabric

Fabric may execute explicitly requested bounded work on workers. Fabric execution should not be conflated with editor-time language analysis.

### Commons / Family Records

Durable analysis, experiment, verification, or promotion artifacts may eventually be published as family records where appropriate. Transient document snapshots and ordinary editor interactions should remain ephemeral unless deliberately promoted into durable records.

## Non-goals for the bootstrap phase

The bootstrap phase did not:

- freeze an agent protocol schema;
- implement an independent parser or semantic model;
- define final incremental-analysis algorithms;
- promise production LSP compatibility;
- claim semantic patch safety;
- couple the language to this service;
- treat MCP as the canonical internal protocol;
- trigger distributed execution from ordinary editor events.

## Implemented topology (Phase 1–3 vertical slice)

The first implementation pass settled the crate topology from real dependency boundaries:

```text
crates/
  service-core/   mncs_service_core
      coords      byte ↔ line/UTF-16 position mapping (single authority)
      document    DocumentStore: workspace discovery, open buffers vs disk,
                  save/close lifecycle, content fingerprints via envelope identity
      analysis    DocumentAnalysis: immutable snapshot binding source identity +
                  generation → SourceFrontEndResult + position map + symbol index
      indexes     SymbolIndex: declarations (AST), references (authoritative
                  NameResolutionIndex), identities/signatures (elaborated Program)
      queries     LanguageService: protocol-neutral query layer and response types
      render      shared hover markdown, semantic-token classification, completion

  lsp/            mncs-lsp binary — tower-lsp projection of the core
  mcp/            mncs-mcp binary — rmcp projection (read-only tools)
```

Dependency direction is strictly one-way: adapters → core → `mncs-language`.
Neither adapter contains language logic; neither protocol's types appear in the
core. Both binaries embed the same `LanguageService` type; running one process
per transport is a deployment choice that does not affect the architecture, and
a resident daemon with multiple client transports remains the long-term target.

### Snapshot model as implemented

```text
content state ──(SourceEnvelope seal)──▶ mncs:source:artifact:<sha256>
                     │
                     ▼
DocumentAnalysis { source_identity, generation, front_end, positions, symbols }
```

`snapshot(uri)` fingerprints current content, returns the resident snapshot on
match, or runs the authoritative frontend once and publishes. A document change
simply makes the next query produce a new snapshot; stale results are never
served because fingerprints are checked before every reuse. Analysis happens
behind a per-document mutex with no global lock held during frontend work.

### Upstream API consumed

One reusable API was added upstream for this slice (see README link): elaboration
now records every name-binding decision as a `NameResolutionIndex`
(`use-site span → declaration span`, classified by declaration kind). The index
is best-effort so partially valid documents stay navigable. The service joins it
against its declaration inventory; it never re-implements scoping.

## MNCS-native query kernel (implemented / experimental)

The first executable MNCS slice is deliberately below the host/service
boundary, not a replacement for it:

```text
authoritative DocumentAnalysis / Program::generate_obligations
                         │ exact bounded status projection
                         ▼
      mncs/status_query.mncs + mncs.core.status.v1
                         │ ReferenceCompiler + research-bytecode
                         ▼
  identity-validated StatusSummary ──┐
                                      ├─ differential comparison
  Rust control StatusCounts ──────────┘
```

The MNCS module owns only deterministic status aggregation that is already
represented by the language standard library. The Rust adapter owns projection
from service records, workspace/library resolution, artifact caching,
execution-budget policy, returned-value validation, response mapping, and the
fail-closed decision. Filesystem access, document lifecycle, locks, LSP/MCP
transport, process management, and authoritative semantic generation remain
Rust responsibilities.

The kernel is compiled and frozen by the real `mncs-language` toolchain, then
reused while its source identity and the resolved `mncs.core.status.v1`
dependency identity are unchanged. It currently selects only the
`mncs-research-bytecode` realization and is bounded to eight obligations. The
MCP `native_obligations` tool exposes this as an experimental read-only query;
it is not the richer Phase 6 native ecosystem protocol.

The conversion record in [`docs/mncs-native-conversion.md`](mncs-native-conversion.md)
tracks what has moved, what remains host-bound, and the evidence-led next
tranche.
