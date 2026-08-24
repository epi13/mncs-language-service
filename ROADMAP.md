# Roadmap

This roadmap intentionally separates architectural establishment from implementation. The MNCS language and compiler are undergoing active development, so the service should first preserve the correct boundaries and only then deepen implementation as authoritative APIs stabilize.

## Status vocabulary

| Label | Meaning |
| --- | --- |
| **Established** | Architectural contract and scope are documented. |
| **Scaffolded** | Repository/module structure exists without substantive implementation. |
| **Implemented / experimental** | Real code path exists but remains research/development grade. |
| **Implemented / exercised** | Real path exists and has been exercised against representative MNCS fixtures. |
| **Deferred** | Intentionally postponed to avoid premature coupling or API freeze. |
| **Blocked / unresolved** | Depends on missing language/service semantics or evidence. |

## Phase 0 — Architectural establishment

**Status: established by this repository bootstrap.**

Goals:

- define the service as a persistent semantic layer rather than an editor-only LSP;
- preserve `mncs-language` as the single semantic authority;
- establish LSP, MCP, and MNCS-native interfaces as adapters over shared state;
- define identity-bound snapshot and stale-state principles;
- preserve `PASS` / `FAIL` / `UNKNOWN` and evidence strength;
- establish the family boundary with Forge, RAVEL, Fabric, and Commons;
- avoid freezing detailed schemas while the language is moving quickly.

Acceptance criteria:

- architecture and protocol documents exist;
- ownership/non-goals are explicit;
- implementation phases are documented without claiming capabilities that do not exist.

## Phase 1 — Resident read-only semantic core

**Status: deferred until suitable `mncs-language` APIs stabilize.**

Initial implementation should emphasize observation rather than mutation.

Target capabilities:

- workspace lifecycle;
- document tracking;
- authoritative frontend invocation;
- versioned analysis snapshots;
- deterministic source position mapping;
- semantic identity lookup;
- symbol/reference/dependency indexing;
- diagnostics and basic semantic queries;
- coarse but correct invalidation;
- cancellation and bounded resource behavior.

The first implementation should prefer semantic correctness and simple invalidation over sophisticated incremental algorithms.

## Phase 2 — First LSP adapter

**Status: deferred.**

Target capabilities:

- document synchronization;
- diagnostics;
- hover;
- document/workspace symbols;
- go-to-definition;
- references;
- semantic tokens;
- completion where the authoritative language model can support it safely.

The adapter should remain thin. Any language understanding discovered to be missing should be pushed into `mncs-language` rather than reimplemented in the LSP layer.

## Phase 3 — First agent/MCP adapter

**Status: deferred.**

Begin with read-only semantic tools over the same resident state.

Candidate capability families:

- workspace/snapshot status;
- identity at position;
- semantic subject description;
- dependencies and dependents;
- diagnostics and causal explanation;
- obligations and evidence;
- authority/effect closure;
- impact inspection;
- compact semantic context assembly.

Exact MCP tool names and schemas should be selected from working experience rather than frozen here.

## Phase 4 — Candidate analysis and semantic impact

**Status: deferred.**

Add isolated candidate snapshots without promoting them to the workspace baseline.

Target capabilities:

- compare baseline and candidate snapshots;
- semantic, authority, obligation, and evidence deltas where supported;
- affected-identity/affected-obligation analysis;
- stale evidence detection;
- bounded candidate validation;
- retained unresolved/unknown state.

This phase should establish safe reasoning about proposed changes before enabling semantic mutation.

## Phase 5 — Safe mutation and semantic patches

**Status: deferred.**

Only begin after snapshot binding and candidate analysis are reliable.

Potential capabilities:

- semantic rename/refactoring;
- identity-bound edits;
- semantic patch representation;
- candidate validation before materializing source edits;
- stale-state refusal;
- explicit separation between proposed, accepted, and promoted changes.

Mutation APIs must fail closed on ambiguity or snapshot mismatch.

## Phase 6 — MNCS-native ecosystem interface

**Status: deferred.**

Develop a richer native interface from demonstrated needs in RAVEL, Forge, Controller, and other MNCS systems.

Possible responsibilities:

- identity-bound task contexts;
- semantic context packets;
- relation/evidence queries not naturally represented in LSP;
- candidate snapshot coordination;
- explicit bounded verification requests;
- family-record references for durable results;
- structured handoff between agents without lossy text-only reconstruction.

The native protocol should remain independent of the transport mechanism selected for deployment.

## Phase 7 — Incrementality, scale, and distributed work

**Status: deferred.**

Only after semantic behavior is stable and measurable:

- fine-grained dependency invalidation;
- persistent caches;
- multi-root workspaces;
- larger-repository indexing;
- bounded distributed verification/execution requests through explicit ecosystem boundaries;
- performance and semantic-density studies for agent context delivery;
- independent clients/adapters for interoperability testing.

## Cross-cutting requirements

Every phase should preserve:

1. one authoritative semantic implementation;
2. exact subject/snapshot identity where available;
3. conservative uncertainty and evidence handling;
4. bounded and cancellable work;
5. clear separation of observation, candidate analysis, and mutation;
6. protocol-neutral internal architecture;
7. no silent broadening of authority;
8. no promotion of transient/editor state into durable evidence without explicit action;
9. useful machine-readable diagnostics and explanation paths;
10. compatibility with the evolving MNCS language rather than accidental lock-in to one source profile.

## Immediate next action

No large implementation push is recommended yet.

When the current `mncs-language` push settles enough to expose a stable frontend/service-facing seam, the next concrete task should be a narrow design-and-implementation pass for Phase 1 that inventories authoritative reusable APIs, identifies only the minimal missing APIs, and builds the first resident read-only semantic snapshot path without duplicating compiler logic.
