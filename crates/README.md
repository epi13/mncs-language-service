# Implementation layout

The first implementation pass established the following crate topology from
observed dependency boundaries:

```text
crates/
  service-core/   mncs_service_core — resident semantic core
  lsp/            mncs-lsp binary   — LSP adapter (tower-lsp)
  mcp/            mncs-mcp binary   — MCP adapter (rmcp), read-only tools
```

Responsibility rules that must be preserved when extending this layout:

- protocol adapters depend inward on `mncs-service-core` only;
- the core depends on authoritative `mncs-language` APIs (`mncs-syntax`,
  `mncs-compiler`, `mncs-model`) and never on an adapter;
- `mncs-language` does not depend on this repository;
- semantic behavior is not duplicated in adapters; shared rendering lives in
  the core so LSP and MCP present identical content;
- protocol wire schemas never become the canonical internal model;
- mutation support stays deferred until identity-bound snapshot and candidate
  analysis semantics are robust.

Representative MNCS fixtures used by every test level live in
[`tests/fixtures/`](../tests/fixtures/) at the repository root.
