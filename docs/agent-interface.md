# Agent Interface Direction

## Purpose

The MNCS Language Service should help coding agents reason about MNCS programs through compact semantic structure rather than forcing them to reconstruct program meaning from repository text on every interaction.

The initial agent-facing interface is expected to be exposed through MCP for interoperability, with a richer MNCS-native interface allowed to emerge later.

## Design goal

The service should answer questions about *meaning, dependency, authority, evidence, and impact* using exact language identities wherever possible.

An agent can already ask (tool names as implemented in `crates/mcp`):

- what semantic subject exists at this source position? → `identity_at_position`;
- describe this exact identity → `describe_subject` (by `identity` or by `uri`+position);
- what does this identity call? who calls it? → `semantic_dependencies` (`outgoing`/`incoming`);
- which obligations/evidence state attaches here? → `obligations` (optionally subject-filtered);
- what authority/effects are involved? → surfaced in `describe_subject`;
- assemble bounded context for this subject → `context_packet` *(experimental)*;
- navigation primitives used by the above → `find_definition`, `find_references`, `list_symbols`, `document_diagnostics`, `workspace_status`.

Snapshot comparison and impact deltas await Phase 4 candidate snapshots; causal diagnostic slices await deeper language support. All tools are read-only and fail closed.

## Why a resident service matters

Agents commonly spend significant context repeatedly searching files, rediscovering definitions, tracing references, and rerunning broad compiler commands.

A resident language service can amortize that work by maintaining:

- exact workspace/document versions;
- semantic identities;
- reference and dependency indexes;
- diagnostics;
- cached authoritative analysis;
- candidate snapshots;
- obligation/evidence relationships.

This should reduce textual rediscovery while improving precision.

## Semantic context packets

A long-term capability may be bounded task-specific context assembly.

Rather than returning an entire file or repository, a request could identify a semantic subject and task class, and the service could return only relevant context such as:

- identity and kind;
- signature/type information;
- contracts and assumptions;
- capabilities/effects;
- callers/callees or other semantic neighbors;
- obligations and evidence state;
- relevant source spans;
- unresolved questions;
- snapshot identity.

The service should not claim that a context packet is sufficient unless the selection policy can justify that claim. Conservative fallbacks may include broader context or an explicit `UNKNOWN`/incomplete status.

## Read before write

The first agent interface should be primarily observational.

A safe maturity sequence is:

1. semantic lookup;
2. dependency/evidence/diagnostic explanation;
3. impact analysis;
4. isolated candidate analysis;
5. identity-bound mutation;
6. semantic patch workflows.

This ordering avoids granting broad write power before stale-state and candidate-validation behavior is trustworthy.

## Candidate changes

Future agent edits should be treated as candidate state first.

Conceptually:

```text
baseline snapshot
      │
      ├── agent proposes candidate
      │
      ▼
candidate snapshot
      │
      ├── authoritative validation
      ├── semantic delta
      ├── authority delta
      ├── obligation/evidence impact
      └── unresolved state
```

The service should distinguish "proposal is syntactically applicable" from "proposal is semantically acceptable" and from "proposal has sufficient evidence for promotion."

## Semantic patches

Where the language can represent a change semantically, future agent workflows should prefer exact identity-bound operations over unconstrained text replacement.

A semantic patch mechanism should eventually be able to state:

- the target identity;
- the expected baseline/snapshot;
- the intended semantic replacement or relation;
- required preserved properties;
- resulting candidate identity/delta;
- validation/evidence state.

Source edits are then one realization of the semantic change rather than the agent's only representation of intent.

No semantic patch format is standardized in this bootstrap phase.

## Authority and trust

Agent requests must not gain authority merely because they originate from a trusted client or model.

The service should preserve language-level authority, capability, evidence, and verification boundaries. An agent may propose or request work; that does not make its claims true or its changes promotable.

## RAVEL relationship

RAVEL may eventually use the service to produce identity-bound agent tasks and handoffs.

A RAVEL task should be able to refer to an exact semantic subject and snapshot instead of requiring each worker to infer the subject from prose and raw files.

This repository should provide the semantic observation boundary; RAVEL retains orchestration responsibility.

## Non-goals

The agent interface should not become:

- a generic shell/filesystem server;
- a replacement for source control;
- an independent verifier;
- an implicit promotion authority;
- an unrestricted repository mutation API;
- a reason to duplicate language semantics outside `mncs-language`.
