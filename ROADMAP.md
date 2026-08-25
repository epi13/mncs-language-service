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

**Status: implemented / exercised** (`crates/service-core`).

Implemented against the current `mncs-language` frontend
(`ReferenceCompiler::front_end`) plus one small upstream addition (the
authoritative `NameResolutionIndex` recorded during elaboration; see the
linked upstream PR in README).

Exercised capabilities:

- workspace discovery and document lifecycle, including unsaved editor buffers overriding disk state (`DocumentStore`);
- authoritative frontend invocation per content change with envelope-identity fingerprints;
- immutable analysis snapshots binding source identity + workspace generation to frontend artifacts;
- deterministic byte ↔ line/UTF-16 position mapping (`coords::PositionMap`, unit-tested for multibyte, CRLF, clamping);
- symbol inventory and reference index joined from AST + name resolutions + elaborated program;
- function-level call graph derived from authoritative call operations;
- obligations via `Program::generate_obligations` with preserved status/freshness/fallbacks;
- structured diagnostics with codes/stages/severities/spans;
- coarse but correct invalidation: changed documents re-analyze from scratch behind per-document locks; unchanged documents reuse resident snapshots (covered by tests).

Not yet implemented: fine-grained incremental invalidation, persistent caches, multi-root workspaces, cancellation tokens (the frontend is fast enough that synchronous bounded work is currently acceptable).

## Phase 2 — First LSP adapter

**Status: implemented / exercised for read features** (`crates/lsp`, binary `mncs-lsp`).

Working, protocol-tested via real JSON-RPC exchanges (`tower-lsp` service driven directly):

- initialize/shutdown and workspace-root configuration;
- full-document sync with open/change/save/close;
- pushed diagnostics mapped through the shared coordinate layer, preserving codes and structured metadata;
- hover (signature, contracts, capabilities/effects, obligation summary, identity — identical content to MCP describe);
- go-to-definition, references (with/without declaration), document highlights;
- nested document symbols including Profile 0.5 records/fields; workspace symbols across documents;
- semantic tokens restricted to authoritatively classified identifiers plus keywords/numbers;
- completion limited to high-confidence contexts (identifier prefixes, nominal-type member namespaces, record fields of typed bindings);
- folding ranges from the CST.

Deliberately omitted: rename/code actions/refactoring (mutation classes), willSave/waitUntil semantics beyond the required minimum, and any expensive-work trigger from keystrokes.

The adapter is thin (~600 lines): it translates between LSP types and core queries only. Language understanding discovered missing was pushed into `mncs-language` (name resolutions), not implemented here.

## Phase 3 — First agent/MCP adapter

**Status: implemented / experimental-to-exercised** (`crates/mcp`, binary `mncs-mcp`).

Read-only tools over the same resident core, protocol-tested with a real MCP client over an in-memory transport:

| Tool | Notes |
| --- | --- |
| `workspace_status` | root, generation, per-document open/analysis currency/validity/diagnostic counts |
| `document_diagnostics` | structured items with dual-coordinate ranges and token expectations |
| `identity_at_position` | explicit declaration-vs-reference roles with resolved targets |
| `describe_subject` | by identity or position: contracts, effects, capabilities, evidence, obligations, call neighborhood, structural members |
| `find_definition` / `find_references` | authoritative resolution only |
| `list_symbols` | document-scoped or workspace-wide with name filter |
| `semantic_dependencies` | `outgoing`/`incoming` call edges lifted from operation-level graph data |
| `obligations` | subject-filterable; preserves PASS/FAIL/UNKNOWN + method + freshness + fallback |
| `context_packet` *(experimental)* | bounded declaration+callee excerpts; `complete=true` only when the outgoing-call closure fit the budget |

Tool results carry structured JSON bound to snapshot identity; failures return explicit structured errors without killing the server. Causal explanation slices and authority/effect closure queries remain future work pending deeper language support.

## Phase 4 — Candidate analysis and semantic impact

**Status: implemented / exercised (initial).**

Isolated candidate snapshots without promoting them to the workspace baseline
are now implemented in the resident core and exposed over MCP.

Working:

- `LanguageService::analyze_candidate(uri, candidate_text)` analyzes proposed
  content in isolation; the workspace baseline snapshot is never modified;
- identity-bound response naming baseline and candidate source identities,
  with an explicit `changed` marker for identical candidates;
- language-owned semantic delta via `Program::semantic_diff` (added / removed /
  changed identities with fingerprints) — no diff semantics are re-implemented
  here;
- obligation deltas (added / removed / status-changed) from authoritative
  obligation generation on both sides, with per-side PASS/FAIL/UNKNOWN counts;
- stale-evidence detection via `Program::invalidation_from`, the language's
  own conservative invalidation report;
- fail-closed behavior: a broken baseline is refused (`unsupported`); a
  candidate that does not elaborate answers with diagnostics only plus an
  explicit unresolved note; nothing is promoted or guessed;
- MCP tool `analyze_candidate(uri, candidate_text)` (read-only).

Update (2026-08, RAVEL-driven): candidates now elaborate against the same
resident resolution as their baseline — workspace documents plus
`MNCS_LIBRARY_PATH` standard-library roots in `StoreResolver` — so editing an
importing module produces true semantic deltas rather than false
unresolvable-import diagnostics. Exercised against the real linked RAVEL
workspace (`crates/service-core/tests/ravel_integration.rs`).

Not yet implemented: cross-document candidate workspaces (the language is
single-module), candidate persistence across restarts, evidence-freshness
joins against external Forge records, and mutation (Phase 5 remains gated on
this foundation).

## Phase 4.5 — Static syntax and GitHub/Linguist readiness

**Status: implemented / exercised (integration prepared; upstream acceptance pending).**

A static presentation layer for environments that cannot run the MNCS
compiler, kept strictly subordinate to the authoritative language:

- canonical TextMate grammar `source.mncs` covering the current Profile
  0.1–0.5 surface (`integration/static-syntax/mncs.tmLanguage.json`);
- grammar structural validation plus a TextMate-subset tokenization engine
  and real-document highlighting tests (`crates/static-syntax`);
- mechanical drift protection against the authoritative lexer: exhaustive
  `TokenKind` mapping (compile-time tripwire), live bidirectional
  conformance tests, manifest count invariant, weekly CI drift watch;
- prepared Linguist language definition, curated licensed sample corpus with
  provenance, upstream PR runbook, and a reproducible validation script that
  exercises a local linguist checkout end to end;
- honest adoption measurement tooling: as of 2026-08, `.mncs` usage (~76
  known public files, one owner) is far below Linguist's ≥2000-file bar, so
  **no upstream PR is submitted yet**.

Details: [`docs/github-language-support.md`](docs/github-language-support.md).
The LSP semantic-token path remains the richer authoritative alternative;
the static grammar never encodes semantics.

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

Phase 4 candidate analysis now has a working identity-bound core exercised by
the MNCS-native RAVEL workspace (`epi13/RAVEL`, `mncs/workspace`): candidate
snapshots, semantic deltas, obligation deltas, and language-owned stale-
evidence detection. Phase 4.5 has the GitHub/Linguist integration prepared
and gated on real adoption. The next pressure points are (a) RAVEL-driven use
of candidate deltas inside its checkpoint flow, (b) multi-root workspaces when
the language grows beyond one module, and (c) Phase 5 semantic patches, which
remain gated until candidate snapshots have survived more real use.
