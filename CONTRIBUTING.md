# Contributing

The MNCS Language Service is now a working resident semantic service with an LSP and MCP adapter. Contributions should preserve the central boundary: this repository keeps MNCS language semantics resident and queryable, but `mncs-language` remains the semantic authority.

## Local checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Integration coverage lives at three levels and new features should extend the appropriate ones:

- `crates/service-core/tests/` — core behavior against `tests/fixtures` MNCS sources (snapshots, invalidation, navigation, obligations);
- `crates/lsp/tests/lsp_protocol.rs` — real JSON-RPC exchanges with the tower-lsp service;
- `crates/mcp/tests/mcp_protocol.rs` — real MCP client/server over in-memory transport.

## Current priorities

Useful contributions now include deepening the implemented phases (see
`ROADMAP.md` for exact statuses): candidate snapshots (Phase 4), causal
diagnostic slices, richer completion contexts where the language can support
them confidently, and performance work backed by measurements.

## Standing prohibitions

- introducing a second parser, type checker, validator, or semantic model;
- freezing protocol schemas without exercised use cases;
- treating MCP or LSP types as the internal service ontology;
- placeholder crates or abstractions with no concrete responsibility;
- write/refactor APIs before snapshot/candidate semantics are established;
- wiring editor events directly to expensive Forge/Fabric/backend work.

## Dependency rule

The intended direction is:

```text
protocol adapters -> service abstractions -> mncs-language
```

`mncs-language` must remain independently usable and must not depend on this repository.

## Claims

Documentation and code should state capability maturity accurately. Distinguish architecture, scaffolded interfaces, experimental implementation, exercised behavior, bounded evidence, and production claims.

Preserve `PASS`, `FAIL`, and `UNKNOWN` rather than converting missing evidence into success.

## Future implementation changes

Once implementation begins, pull requests should explain:

- which service responsibility is being added;
- which authoritative `mncs-language` API is used;
- whether any new upstream API is required;
- snapshot/invalidation behavior;
- trust/evidence implications;
- protocol-specific behavior versus shared service behavior;
- tests or fixtures that exercise the boundary.
