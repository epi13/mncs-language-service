# Contributing

The MNCS Language Service is currently in an architecture-first bootstrap phase.

Contributions should preserve the central boundary: this repository keeps MNCS language semantics resident and queryable, but `mncs-language` remains the semantic authority.

## Current priorities

Until the first resident semantic core is intentionally started, useful contributions include:

- clarifying architectural boundaries;
- identifying service-facing API needs in `mncs-language`;
- documenting LSP/MCP/native projection requirements;
- defining conservative snapshot, invalidation, and stale-state behavior;
- developing representative future integration scenarios;
- improving trust/evidence and family-integration documentation.

## Avoid premature implementation

During the bootstrap phase, avoid:

- introducing a second parser, type checker, validator, or semantic model;
- freezing detailed protocol schemas without exercised use cases;
- treating MCP or LSP types as the internal service ontology;
- adding placeholder crates or abstractions with no concrete responsibility;
- implementing write/refactor APIs before snapshot/candidate semantics are established;
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
