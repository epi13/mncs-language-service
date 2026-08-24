# Future implementation layout

This directory reserves the implementation space for the MNCS Language Service without freezing a crate topology before the service-facing `mncs-language` APIs are ready.

The expected responsibility groups are:

```text
resident service core
service query/protocol model
LSP adapter
MCP adapter
future MNCS-native adapter
```

These may become separate crates, combined crates, or a different arrangement after the first implementation pass inventories actual dependency boundaries.

The architectural constraints are more important than crate names:

- protocol adapters depend inward on shared service abstractions;
- the service depends on authoritative `mncs-language` APIs;
- `mncs-language` does not depend on this repository;
- semantic behavior is not duplicated in adapters;
- protocol wire schemas do not become the canonical internal semantic model by convenience;
- mutation support is deferred until identity-bound snapshot and candidate analysis are robust.

Do not add placeholder Rust crates solely to make the repository look implemented. The first crate structure should emerge from a concrete Phase 1 resident-read-only implementation pass.
