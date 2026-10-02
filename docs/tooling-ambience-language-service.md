# Tooling ambience: mncs-language-service contribution

Provider-owned note for the environment-wide tooling ambience
inventory. Held here (not in a shared registry) until the Debug
campaign's inventory artifact lands, then merged into whatever
schema/vocabulary it establishes. No competing registry is created.

```text
tool:
  mncs-language-service

owned coherence:
  semantic workspace state (resident snapshots, generations,
  diagnostics, obligations, dependency relationships, events)

target ambience:
  A3 continuous semantic coherence

triggers:
  document open/change/save/close, filesystem refresh,
  toolchain binding change, workspace reconfiguration

reuse identity:
  workspace root + source identities + semantic generation +
  toolchain digest + service build/instance + event stream/cursor

normal output:
  none (quiet current) or compact actionable delta
  (counts + admitted findings + expansion handles)

deep state:
  resident and queryable (diagnostics, graph, obligations,
  capabilities, candidate analysis, debug binding)

mutations:
  none to source through ambient semantic observation

explicit boundaries:
  rename, quickfix application, refactor, import insertion,
  formatting, and candidate application require explicit intent
  with proper claims/authority; ambient observation never writes
```

## Composition map (current)

```text
Environment declares probe + recovery (service contract)
  -> Doctor probes resident-status, reconciles via resident-reconcile
  -> semantics pass diffs persisted observations vs durable cursors
  -> poll/capsule on change; nothing on quiet

mncs-test consumes impact facts (future: narrower invalidation)
mncs-debug consumes debug_source_binding + impact (no Debug authority taken)
projections converge through shared state/events (no ad hoc callbacks)
```

## Evidence

- Provider: `tools/mnls_provider.py` + `tools/test_mnls_provider.py`
- Manifest: `.mncs/project.json` resident/semantic invocations
- Core: `crates/service-core/src/ambient.rs`,
  `mncs/semantic_capsule.mncs`
- Tests: `ambient::tests`, `ambient_coherence.rs`
- Environment composition: `mncs_env/semantics.py`,
  `samples/ambient-semantic.environment.json`
- Full contract: `docs/ambient-semantic-coherence.md`
