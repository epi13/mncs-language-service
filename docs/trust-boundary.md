# Trust Boundary

## Principle

The MNCS Language Service is not trusted to invent or strengthen language semantics.

It is trusted only to faithfully manage resident workspace state, invoke authoritative language APIs, retain identities, project results through adapters, and conservatively handle uncertainty and stale state.

## Authoritative sources

`mncs-language` remains authoritative for:

- syntax and source-profile meaning;
- semantic models and identities;
- validation rules;
- compiler-stage semantics;
- obligation generation;
- verification semantics;
- evidence interpretation owned by the language;
- IR and backend contracts.

The service may cache and index results from these APIs but must not silently reinterpret them.

## Service-owned state

The service may own operational state such as:

- open document buffers;
- workspace membership;
- document versions;
- snapshot references;
- cache entries;
- query indexes;
- client sessions;
- cancellation state;
- adapter configuration;
- candidate workspaces.

Operational state must remain distinguishable from language-semantic identity.

## Stale state

Future mutation and candidate APIs should fail closed when their expected subject identity or baseline snapshot no longer matches current state.

The safe outcome for ambiguity is refusal or `UNKNOWN`, not best-effort mutation.

## Evidence handling

The service must preserve evidence provenance and strength.

It must not:

- treat cached evidence as current when the subject identity changed;
- convert bounded execution agreement into universal equivalence;
- convert `UNKNOWN` into `PASS`;
- imply verification occurred merely because parsing or validation succeeded;
- promote agent assertions into language facts;
- claim independent verification when only the service/compiler path was exercised.

## Adapter trust

LSP, MCP, and MNCS-native adapters are presentation/transport boundaries.

An adapter may omit information that a client cannot represent, but omission must not strengthen the meaning of the remaining result.

For example, if an editor UI cannot render a full evidence graph, it may show a conservative diagnostic or status summary; it must not display a verified-success state that the underlying result does not justify.

## Agent trust

Agents are untrusted semantic producers by default.

They may:

- ask questions;
- propose candidates;
- generate patches;
- suggest proofs or evidence;
- request explicit verification work.

Their outputs must pass the same authoritative validation/evidence boundaries as any other producer.

## External execution

Forge, Fabric, backend tools, solvers, and independent verifiers may eventually be reachable through explicit service requests or ecosystem coordination.

Those systems retain their own trust and evidence roles. The service should record or forward their results with exact provenance rather than laundering them into a generic success bit.

## Mutation trust boundary

Before the service performs semantic mutation, it should be able to answer:

1. Which exact snapshot is being changed?
2. Which exact semantic identity is targeted?
3. Does the target still match the expected baseline?
4. What candidate state results?
5. What semantic/authority/evidence obligations changed?
6. Which checks ran, and which did not?
7. Is the operation merely proposed, accepted into the workspace, or promoted into a stronger durable state?

If those distinctions cannot be retained, the mutation mechanism is not mature enough to claim semantic safety.

## Durable records

Ordinary editor activity and transient snapshots are not automatically Family Records or durable evidence.

Persistence into Commons or another durable MNCS record should be deliberate and should identify the exact subject, producer, method, scope, and evidence state being retained.
