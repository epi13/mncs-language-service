# MNCS Language Service

`mncs-language-service` is the planned persistent semantic service for the MNCS language.

It is intended to expose the same authoritative MNCS language semantics to humans, editors, coding agents, Forge, RAVEL, and other MNCS components through multiple protocol adapters, while keeping language semantics owned by [`mncs-language`](https://github.com/epi13/mncs-language).

This repository is intentionally in an **architecture-first bootstrap phase**. The MNCS language itself is moving quickly, so this repository currently establishes boundaries, concepts, interfaces, and a buildable direction without prematurely freezing unstable implementation details.

## Mission

Provide a resident, identity-aware semantic interface to the MNCS language for humans, agents, editors, and MNCS ecosystem components.

The service is not a second compiler and must not become a second implementation of MNCS semantics.

## Architectural role

```text
                         clients
        ┌──────────────────┼──────────────────┐
        │                  │                  │
      editors            agents          MNCS systems
        │                  │                  │
       LSP                MCP           MNCS-native API
        └──────────────────┼──────────────────┘
                           │
                 MNCS Language Service
                           │
          resident workspace + semantic state
                           │
                     mncs-language
                           │
          syntax / semantics / compiler / IR
```

The service should eventually maintain resident workspace state, identity-bound analysis snapshots, semantic indexes, dependency relationships, diagnostics, obligations, evidence state, impact information, and safe semantic interaction surfaces.

LSP, MCP, and future MNCS-native protocols are adapters over that shared semantic service rather than separate implementations.

## Ownership boundary

### `mncs-language` owns

- source syntax and parsing semantics;
- canonical semantic models;
- validation rules and diagnostics;
- semantic identities;
- compiler architecture and lowering;
- IR and backend contracts;
- verification semantics and obligation generation;
- language-owned evidence and experiment artifacts.

### `mncs-language-service` owns

- resident workspace and document state;
- incremental orchestration over authoritative language APIs;
- semantic snapshots and caches;
- source-position to semantic-identity navigation;
- symbol, dependency, and reference indexes;
- semantic query infrastructure;
- protocol adaptation for editors and agents;
- interaction policy around stale snapshots and candidate changes;
- service observability and lifecycle.

A language semantic capability required by the service should be added to `mncs-language` and consumed here rather than reimplemented here.

## Intended interfaces

### LSP

A standards-compatible editor interface for diagnostics, hover, completion, navigation, semantic tokens, refactoring, code actions, and related IDE features.

### MCP / agent interface

A structured agent-facing surface for operations such as identity lookup, semantic description, dependency inspection, obligation/evidence queries, impact analysis, diagnostic explanation, and eventually candidate semantic patches.

MCP is an adapter, not the internal architecture.

### MNCS-native interface

A future richer interface for RAVEL, Forge, Controller, and other MNCS components. It may expose concepts that do not map cleanly to editor protocols, including semantic identities, snapshots, authority envelopes, evidence state, relations, candidate deltas, and bounded verification state.

## Core principles

1. **One semantic authority.** The service consumes `mncs-language`; it does not redefine MNCS.
2. **Persistent semantic state.** Repeated queries should reuse resident workspace analysis rather than reconstructing the program from scratch.
3. **Identity-bound interaction.** Queries and future mutations should be tied to exact source/semantic snapshots where possible.
4. **Human and machine symmetry.** Editors and agents should inspect the same underlying semantic structure through role-appropriate representations.
5. **PASS / FAIL / UNKNOWN preservation.** The service must not convert missing or bounded evidence into stronger claims.
6. **Bounded work.** Expensive verification, backend execution, Forge search, or Fabric work must remain explicit rather than being triggered casually by keystrokes.
7. **Protocol independence.** Internal service concepts should not be dictated by LSP, MCP, JSON-RPC, or any one client transport.
8. **Fail closed on stale state.** Future mutation/refactoring APIs should refuse identity or snapshot mismatches rather than silently applying ambiguous text changes.
9. **Semantic density for agents.** The service should help agents request compact, task-relevant semantic context instead of repeatedly rereading whole repositories.
10. **No premature API freeze.** Early structure should remain intentionally adaptable while the language and compiler are undergoing rapid development.

## Intended layering

The precise crate split is intentionally deferred, but the expected layering is:

```text
protocol adapters
    ├── LSP
    ├── MCP
    └── MNCS-native
          │
          ▼
service protocol / query model
          │
          ▼
resident service core
          │
          ▼
mncs-language authoritative APIs
```

See [`docs/architecture.md`](docs/architecture.md), [`docs/protocol-model.md`](docs/protocol-model.md), and [`ROADMAP.md`](ROADMAP.md).

## Current status

**Scaffolded / architecture established.**

No production language server, MCP server, or native MNCS service is claimed yet. This repository currently exists to make the intended system boundaries explicit so implementation can begin cleanly once the corresponding `mncs-language` APIs stabilize enough to support it.

## Relationship to the MNCS family

- **MNCS Language** defines the language semantics and compiler-facing artifacts.
- **MNCS Language Service** keeps those semantics resident and queryable for humans and machines.
- **Forge** may consume semantic state, obligations, and candidate analysis but does not become the semantic authority.
- **RAVEL** may coordinate agents using identity-bound tasks and semantic context supplied by the service.
- **Fabric** may execute explicitly requested bounded work; it is not part of editor-time analysis.
- **Commons / Family Records** may persist durable results, but transient editor state should not automatically become durable family evidence.

## License

Apache-2.0.
