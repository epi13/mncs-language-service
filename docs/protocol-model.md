# Protocol Model

## Goal

The service should expose one shared semantic interaction model through multiple protocol adapters without allowing any one external protocol to define the internal architecture.

LSP, MCP, and a future MNCS-native interface serve different clients and should remain projections over the same resident semantic state.

## Core principle

> Protocols transport MNCS language-service meaning; they do not define it.

The internal query model should be capable of representing exact subject identity, workspace/snapshot identity, semantic relationships, diagnostics, obligation/evidence state, and explicit unresolved outcomes even when a specific adapter cannot surface every field directly.

## LSP projection

LSP is the compatibility interface for editors and IDEs.

Implemented mappings (see `crates/lsp`):

- full-text document sync; pushed diagnostics preserving codes with structured `data` (stage, expected/found tokens);
- hover rendered from the same core content the MCP describe tool returns;
- go-to-definition, references, document highlights via authoritative resolution;
- nested document symbols; workspace symbols; folding ranges from the CST;
- semantic tokens over a service-owned legend (`module, function, parameter, variable, type, variant, field, keyword, number`) restricted to authoritatively classified identifiers;
- completion in high-confidence contexts only.

Deferred until mutation safety matures: rename, code actions, refactoring.
Richer MNCS metadata remains available to editor clients through experimental
`mncs/*` methods declared in the server capabilities.

## MCP / agent projection

MCP provides an interoperable agent-facing adapter to the same service
(`crates/mcp`). The initial emphasis is read-only semantic inspection.

Implemented tools: `workspace_status`, `document_diagnostics`,
`identity_at_position`, `describe_subject`, `find_definition`,
`find_references`, `list_symbols`, `semantic_dependencies`, `obligations`, and
the experimental bounded `context_packet`. See `docs/agent-interface.md` for
request/response semantics and uncertainty handling.

Tool responses carry a structured JSON payload alongside a short text summary.
Every structured payload names its snapshot. Failures are returned as explicit
structured errors (`is_error` + reason), never as empty successes, and never
crash the server.

MCP has not become the canonical in-process data model: the wire schemas are
derived from the core query types, not the other way around.

## MNCS-native projection

A future native interface may expose concepts that are awkward or lossy through LSP or general-purpose MCP tools.

Candidate concepts include:

- exact semantic identities and relation identities;
- identity-bound task subjects;
- workspace and candidate snapshot identities;
- authority envelopes;
- evidence and assurance state;
- semantic/authority/evidence deltas;
- bounded verification state;
- candidate analysis and promotion boundaries;
- semantic patches and stale-state refusal;
- compact machine-oriented semantic context.

The native protocol should emerge from proven MNCS interaction needs rather than being designed speculatively in the bootstrap phase.

## Query semantics

The eventual service query model should favor structured semantic operations over text-oriented convenience operations.

For example, the useful primitive is not merely "grep for this token" but "return semantic dependents of this exact identity for this exact snapshot."

Likewise, future edits should prefer identity-bound candidate operations over unconstrained text replacement when the language can express the change semantically.

## Snapshot binding

Where practical, responses should identify the source/workspace snapshot they describe.

Future state-changing requests should be able to reject stale inputs when the requested subject or baseline no longer matches current state.

A conceptual interaction is:

```text
client query
  subject identity
  expected snapshot
        │
        ▼
service resolves against resident state
        │
        ├── match -> answer / candidate
        └── mismatch -> explicit stale/unresolved result
```

The exact identifiers and wire representation should reuse `mncs-language` identities whenever those identities already exist.

## Result strength

Protocol results must preserve strength and uncertainty.

Adapters must not collapse:

- `PASS`, `FAIL`, and `UNKNOWN`;
- verified and merely observed behavior;
- current and stale evidence;
- authoritative and candidate state;
- facts and preferences;
- bounded agreement and universal equivalence.

If a client protocol cannot express a distinction directly, the adapter should choose conservative presentation rather than silently strengthening the result.

## Context delivery for agents

A first experimental version of bounded context assembly exists:
`context_packet(uri, identity, max_excerpts)` returns the subject's declaration
excerpt plus callee excerpts within budget, together with a `complete` flag that
is true **only** when the whole outgoing-call closure fit inside the budget, and
`notes` explaining any shortfall. The service claims neither minimality nor
completeness beyond this check. Task-class-aware selection policies remain
future work.

## Mutation boundary

Read-only queries should precede mutation in implementation order.

Before semantic refactoring or patch application is considered mature, the service should have:

- stable snapshot binding;
- exact target identity resolution;
- stale-state refusal;
- candidate isolation;
- semantic delta calculation;
- validation against authoritative language APIs;
- clear distinction between proposal and promotion.

Until then, adapters should remain conservative about write capabilities.
