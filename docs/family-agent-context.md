# Bounded family agent context

`family_agent_context` is the family entry query for an agent entering an MNCS
repository. It is a read-only composition surface owned by
`mncs-language-service`; it does not make the service a second owner of
language, Commons, or Atlas semantics.

The response schema is `mncs.family-agent-context/2` and carries:

- the repository-owned `.mncs/project.json` declaration, byte identity, and
  Standard-owned conformance/validation identity;
- the current `mncs-language` profile, capability content identity, compiler
  inventory identity, filtered capability facts, and an optional identity
  delta;
- the Commons-validated architecture schema/content/validation identities,
  relevant ownership, canonical paths, active shadows, generators, and an
  optional retained architecture delta;
- the repository-owned verification-obligation inventory identity, bounded
  obligation summaries, lifecycle/executor metadata, and an explicit state;
- bounded unresolved language pressures from the Commons projection, bound to
  both the pressure registry identity and a freshly checked generated view;
- an optional Atlas project summary explicitly labelled non-normative
  orientation;
- bounded negative knowledge derived from lifecycle/ownership identities, such
  as reference-only parity or temporary migration surfaces that are not
  ordinary canonical paths; and
- per-source provenance plus `complete`, `partial`, or `unknown` completeness.

The source boundaries are intentional:

| Fact | Authority | Role in this query |
| --- | --- | --- |
| Language profile, exports, effects, compiler inventory | `mncs-language` | authoritative source projection |
| Family ownership, canonical paths, shadows, generators | `MNCS-Commons` | authoritative architecture projection and delta owner |
| Pressure lifecycle and bounded relevant-pressure rows | `MNCS-Commons` | authoritative registry/view projection and lifecycle owner |
| Repository identity, contracts, verification obligations | repository-local manifest and its referenced inventory | repository-owned declarations |
| Human orientation and related-project summary | `mncs-atlas` | optional, non-normative projection |
| Bounded composition and identity/delta envelope | Language Service | query interface only |

`known_language_identity` and `known_architecture_identity` use the same
protocol shape for incremental entry:

```text
same identity        -> unchanged envelope
retained identity    -> bounded delta plus current relevant facts
unknown/old identity -> bounded full projection with an explicit limitation
```

Language Service does not discover or interpret Commons' raw architecture
model, delta history, pressure records, or generated views. It invokes
Commons' bounded `family agent-context` projection and only composes its
versioned response envelope. Likewise, manifest validity comes from the
Standard validator rather than a second schema implementation.

`complete` requires a verified local manifest, a current language/compiler
projection, a current and validated Commons architecture projection, and a
current pressure projection whose registry and generated view identities are
present. A declared obligation inventory is independently reported as
`current`, `truncated`, `invalid`, `unavailable`, or `not_declared`; its
absence never causes the service to invent obligations. An absent manifest,
unavailable authority, stale identity, invalid source, or truncated collection
preserves `UNKNOWN`/`partial`; Atlas absence does not reduce authoritative
completeness because Atlas is orientation-only.

The query is intentionally not a repository dump. Use semantic queries such as
`context_packet`, `describe_subject`, and `semantic_dependencies` for a source
subject after this family preflight has established the governing identities.
