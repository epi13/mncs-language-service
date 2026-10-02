# Ambient semantic coherence

The Language Service is the environment's ambient semantic-perception
layer: it keeps authoritative semantic state resident, advances a
semantic generation as the workspace changes, and serves bounded
observations so agents start from *what changed and what needs
attention* instead of reconstructing the repository from files.

This document describes the provider-owned contracts Environment and
other family components compose with. It does not describe editor
behavior; editors are one client of the same resident core.

## Authority boundary

```text
mncs-language
    syntax, semantics, compiler truth (diagnostics, graphs, obligations)

mncs-language-service
    resident semantic state, identity-bound snapshots, bounded
    projections (status, events, capsule, impact, candidate analysis)

mncs-environment
    lifecycle composition, session context, durable cursors

mncs-test
    verification semantics (what proves correctness)

mncs-debug
    runtime diagnostic semantics (what happened, why it failed)

Doctor
    health and recovery orchestration

Forge
    bounded execution and continuous supervision
```

The service publishes semantic facts. It never decides which tests
prove correctness, what action should execute, or whether a failure
explains a fault. Ambient observation is read-only: analysis,
indexing, diagnostics, impact, candidate evaluation, and capsules may
run automatically; rename, quickfix, refactor, import insertion, and
formatting require explicit intent.

## Resident lifecycle (provider-owned)

`tools/mnls_provider.py` is the single provider-owned operator. All
socket paths, lease files, host-binary discovery, process supervision,
and toolchain measurement live there; callers pass a workspace root
and receive bounded JSON.

| Op | Effect | Meaning |
|----|--------|---------|
| `status` | read | Probe: converge to disk, then state, identities, generation, stream, cursor, totals |
| `ensure` | execute | Bounded reconcile: attach, or start when absent/stale |
| `stop` | execute | Stop only a provider-owned lease; never foreign |
| `poll` | read | Resume the event stream (stream identity required) |
| `capsule` | read | Fetch the bounded semantic capsule |
| `query` | read | One allowlisted read-only RPC (no edits, no source dumps) |

Environment binds these through descriptor addressing as
`mncs-language-service:resident-status`,
`mncs-language-service:resident-reconcile`,
`mncs-language-service:semantic-poll`,
`mncs-language-service:semantic-capsule`, and
`mncs-language-service:semantic-query` (see `.mncs/project.json`).
A definition declares the probe plus recovery as an Environment
service; Doctor probes, reconciles, and re-probes under existing
session authority. Environment never learns Cargo commands, socket
paths, or launch details.

Safety rules (tested in `tools/test_mnls_provider.py`):

- A live socket is trusted only when it reports the requested
  canonical workspace root. A foreign host is reported, refused,
  and never killed.
- `ensure` restarts only provider-owned leases, and only when the
  resident toolchain binding drifted or the socket is dead.
- A missing host binary reports `host-unavailable` with an exact
  build hint instead of guessing.
- `stop` is idempotent and verifies the leased instance before
  signaling; interrupted starts are adopted only when the live
  socket reports the same workspace.
- Every probe first reconciles the resident to disk truth through
  `refresh_workspace`: shell-made edits bypass LSP notifications,
  so a probe that only reads resident state would report stale
  generations as current. Refresh is idempotent and bounded
  (quiet workspaces pay reads only; only actual changes analyze),
  converges state rather than diverging it, and mutates no source,
  which is why the probe keeps read effects. The reconciler's
  event source converges the same way before polling.

## Identity model

Resident state binds to all of:

- workspace root (canonicalized; the checkpoint, socket, and lease
  live under `<root>/.mncs/`);
- toolchain binding (`MNCS_LANGUAGE_ROOT`, `MNCS_LIBRARY_PATH`,
  `MNLS_TOOLCHAIN_IDENTITY` pin), digested and echoed by the host;
- service build fingerprint (version + executable path + mtime);
- process instance id (random per host, distinct across restarts);
- semantic generation (workspace change counter);
- event stream identity + cursor.

Two worktrees never share state: different roots mean different
sockets, leases, checkpoints, streams, and generations
(`ambient_coherence.rs::separate_workspaces_never_share_semantic_state`).
Two consumers of one identical workspace generation share one
resident service safely through independent cursors.

A restart under the same toolchain restores stream continuity from
the durable checkpoint and publishes `reconciled` events for offline
changes. A restart under a different toolchain keeps generation
continuity but starts a fresh stream epoch: saved cursors refuse
instead of resuming silently. Equivalence is never inferred from
paths or bytes alone.

## Events and cursors

`poll_events` serves the bounded `mncs.workspace-change/2` stream.
A nonzero cursor without its stream identity is refused; aged-out
history answers `reset_required` with no events. Consumers resume
with `(stream_identity, after_cursor)` and reconcile through the
capsule on reset. Restart-safe behavior is covered by
`ambient_coherence.rs` (resume, refusal, foreign-stream refusal,
toolchain-change epoch).

## Semantic capsule

`semantic_capsule(known_stream_identity, known_cursor)` answers the
immediate structural questions cheaply:

- current generation, stream, and cursor;
- whether the event window resumed or reconciled;
- measured totals (diagnostics, changed subjects, obligations,
  affected modules, envelope truncation);
- admitted findings (bounded to 20 by policy), each with kind,
  relevance, severity, summary, locator, and an exact expansion
  handle naming the follow-up RPC;
- policy evidence (backend, kernel/artifact identities, counts,
  completeness verdict).

Admission is decided by `mncs/semantic_capsule.mncs`, executed
through the authoritative compiler and research-bytecode backend.
Relevance classification, budgets, overflow accounting, and the
completeness verdict live in MNCS; the host measures ranks,
projects the fixed 32-finding envelope in deterministic priority
order, and applies admission flags mechanically. There is
intentionally no Rust control reimplementation: correctness is
established by executed-behavior tests (`ambient::tests`, plus
`ambient_coherence.rs` end-to-end). When the policy is unavailable
or its verdict incomplete, the capsule answers `Unsupported` with
measured totals and no findings rather than guessing.

A quiet workspace produces an empty actionable capsule: no source
bodies, no reference lists, no symbol indexes, no hover data. Deep
state stays resident and reachable through expansion handles.

## Semantic impact

`semantic_impact(uri, identity)` returns single-hop dependency
edges, the bounded compiler-owned impact neighborhood (depth 2,
256 nodes), and the obligations whose subjects fall inside the
affected set. It mirrors the event-path impact projection so
ambient consumers, verification owners, and Debug share one
vocabulary. Unknown identities answer with an empty incomplete
neighborhood rather than an error.

## Candidate analysis

`analyze_candidate(uri, candidate_text)` evaluates a speculative
edit without mutating the workspace baseline (generation and disk
state unchanged; verified live and in `mcp_protocol.rs`). It is
available over the socket and MCP surfaces. The provider `query`
transport deliberately excludes it: candidate bodies do not belong
in argv. See LS-P-007 for the file-artifact follow-up.

## Language capabilities

`language_capabilities` serves the content-addressed index
generated by `mncs-language`. Agents must use topic/symbol/profile
targeted retrieval (measured 13 KB) instead of full dumps
(measured 115 KB). Delta queries (`delta_from`, `known_identity`)
keep repeated lookups compact. Capability facts are never inlined
into ambient context automatically.

## Family-agent context

`family_agent_context` remains the bounded family-orientation
projection (repository manifest, language, architecture, pressures,
verification obligations). It complements the semantic capsule
rather than duplicating it: orientation versus current workspace
state. Measured at 57 KB against a ~2 KB capsule, it is a
deliberate deep query, not ambient context.

## Debug composition

`debug_source_binding` projects exact source snapshots and
compiler-owned declaration/test identities into
`mncs.debug-source-binding/1` for `mncs-debug` consumers, with
explicit capability states for runtime operation resolution,
failure locations, and breakpoints. The service answers *where a
subject is in source*; Debug answers *what happened*. Operation
spans resolve from the compiler-owned execution source map; a
resolved span is a navigation anchor, never a claim that the
runtime can suspend there.

## Diagnostics as state

Diagnostics are structured current state: stable content-bound
snapshots, causal related locations, and event deltas (added /
resolved codes). Ambient context carries counts and deltas; full
per-document diagnostics stay behind `document_diagnostics`.
