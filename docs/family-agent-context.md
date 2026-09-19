# Bounded family agent context

`family_agent_context` is the family entry query for an agent entering an MNCS
repository. It is a read-only composition surface owned by
`mncs-language-service`; it does not make the service a second owner of
language, Commons, or Atlas semantics.

The response schema is `mncs.family-agent-context/1` and carries:

- the repository-owned `.mncs/project.json` declaration and its byte identity;
- the current `mncs-language` profile, capability content identity, compiler
  inventory identity, filtered capability facts, and an optional identity
  delta;
- the Commons architecture schema/content identity, relevant ownership,
  canonical paths, active shadows, generators, and an optional retained
  architecture delta;
- bounded unresolved language pressures from the Commons projection;
- an optional Atlas project summary explicitly labelled non-normative
  orientation; and
- per-source provenance plus `complete`, `partial`, or `unknown` completeness.

The source boundaries are intentional:

| Fact | Authority | Role in this query |
| --- | --- | --- |
| Language profile, exports, effects, compiler inventory | `mncs-language` | authoritative source projection |
| Family ownership, canonical paths, shadows, generators, pressures | `MNCS-Commons` | authoritative coordination projection |
| Repository identity, contracts, test obligations | repository-local manifest | repository-owned declaration |
| Human orientation and related-project summary | `mncs-atlas` | optional, non-normative projection |
| Bounded composition and identity/delta envelope | Language Service | query interface only |

`known_language_identity` and `known_architecture_identity` use the same
protocol shape for incremental entry:

```text
same identity       -> unchanged envelope
retained identity   -> bounded delta plus current relevant facts
unknown/old identity-> bounded full projection with an explicit limitation
```

An absent manifest, unavailable authority, stale identity, or truncated
collection never becomes a successful claim. The response preserves
`UNKNOWN` through `completeness.state` and lists the reason in
`completeness.limitations`.

The query is intentionally not a repository dump. Use semantic queries such as
`context_packet`, `describe_subject`, and `semantic_dependencies` for a source
subject after this family preflight has established the governing identities.
