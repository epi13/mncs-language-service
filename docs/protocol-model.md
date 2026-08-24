# Protocol Model

## Goal

The service should expose one shared semantic interaction model through multiple protocol adapters without allowing any one external protocol to define the internal architecture.

LSP, MCP, and a future MNCS-native interface serve different clients and should remain projections over the same resident semantic state.

## Core principle

> Protocols transport MNCS language-service meaning; they do not define it.

The internal query model should be capable of representing exact subject identity, workspace/snapshot identity, semantic relationships, diagnostics, obligation/evidence state, and explicit unresolved outcomes even when a specific adapter cannot surface every field directly.

## LSP projection

LSP is the compatibility interface for editors and IDEs.

Expected eventual mappings include:

- diagnostics;
- hover;
- completion;
- go-to-definition;
- references;
- document/workspace symbols;
- semantic tokens;
- inlay hints;
- code actions;
- rename/refactoring once mutation safety is mature.

LSP-facing output may be intentionally human-oriented, but richer MNCS metadata should remain available through structured extension fields or companion queries where appropriate.

## MCP / agent projection

MCP should provide an interoperable agent-facing adapter to the same service.

The initial emphasis should be read-only semantic inspection rather than broad mutation.

Potential capability families include:

- workspace and snapshot status;
- identity at source position;
- describe semantic subject;
- references, dependencies, and dependents;
- diagnostics and explanation slices;
- obligations and evidence state;
- authority/effect relationships;
- semantic impact queries;
- compact task-specific context assembly.

Exact tool names and schemas are intentionally deferred until the underlying language APIs and service query model are sufficiently stable.

MCP must not become the canonical in-process data model merely because it is convenient for early agent integration.

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

One long-term opportunity is task-specific semantic context generation.

Instead of forcing an agent to repeatedly reread a repository, the service may eventually assemble a bounded context packet around a subject and task, containing only relevant semantic neighbors, dependencies, contracts, authority, obligations, evidence, and source spans.

This is a future capability, not a bootstrap commitment, but it is a major reason to preserve protocol-neutral semantic state from the beginning.

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
