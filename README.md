# MNCS Language Service

`mncs-language-service` is the resident semantic service for the MNCS language.

It exposes the same authoritative MNCS language semantics to editors, coding agents, and other MNCS components through multiple protocol adapters (LSP and MCP today), while keeping language semantics owned by [`mncs-language`](https://github.com/epi13/mncs-language).

The service maintains **resident workspace state**: it tracks documents, runs the authoritative `mncs-language` frontend once per content state, binds the results into identity-bound analysis snapshots, indexes them for navigation and semantic inspection, and answers repeated queries without recomputing anything.

## Status

**Phase 1–3 first implementation: working vertical slice (implemented / exercised).**

```text
MNCS source
   ↓
resident authoritative analysis   (mncs-syntax → mncs-compiler → mncs-model)
   ↓
identity-bound snapshot           (mncs:source:artifact:<sha256> + workspace generation)
   ↓
shared semantic query core        (mncs-service-core)
   ├── LSP  → mncs-lsp            editor diagnostics/navigation/hover/tokens/completion
   └── MCP  → mncs-mcp            agent semantic inspection (read-only)
```

What works today:

- document lifecycle (open/change/save/close) with unsaved editor buffers overriding disk;
- authoritative parsing/elaboration/validation through `ReferenceCompiler::front_end`;
- immutable snapshots bound to exact source identities with correct coarse invalidation;
- structured diagnostics preserving codes/stages/severities/spans;
- hover, go-to-definition, references, highlights — all from authoritative name resolution, never text search;
- document/workspace symbols including Source Profile 0.5 record types and fields;
- semantic tokens, conservative completion, folding ranges;
- call-graph dependencies/dependents derived from elaborated bodies;
- obligations with preserved `PASS` / `FAIL` / `UNKNOWN` status;
- a read-only MCP tool surface for agents over the same resident state.

What is explicitly not implemented yet: cross-module workspaces (the language itself is single-module today), candidate snapshots and impact deltas (roadmap Phase 4), mutation/semantic patches (Phases 5+), incremental fine-grained invalidation, and any Forge/Fabric/backend integration.

See [`ROADMAP.md`](ROADMAP.md) for the authoritative status vocabulary.

## Architectural role

```text
                         clients
        ┌──────────────────┼──────────────────┐
        │                  │                  │
      editors            agents          MNCS systems
        │                  │                  │
       LSP                MCP           MNCS-native API
        └──────────────────┼──────────────────┘
                           │
                 MNCS Language Service
                           │
           resident workspace + semantic state
                           │
                     mncs-language
                           │
           syntax / semantics / compiler / IR
```

LSP and MCP are adapters over one shared resident core. Neither protocol defines the internal ontology; both resolve the same subjects to the same identities and snapshots.

## Ownership boundary

### `mncs-language` owns

- source syntax and parsing semantics;
- canonical semantic models;
- validation rules and diagnostics;
- semantic identities;
- compiler architecture and lowering;
- IR and backend contracts;
- verification semantics and obligation generation;
- language-owned evidence and experiment artifacts.

### `mncs-language-service` owns

- resident workspace and document state;
- orchestration over authoritative language APIs;
- identity-bound analysis snapshots and caches;
- source-position to semantic-subject navigation;
- symbol, dependency, and reference indexes derived from authoritative artifacts;
- semantic query infrastructure;
- protocol adaptation for editors and agents;
- interaction policy around stale snapshots and candidate changes;
- service observability and lifecycle.

A language semantic capability required by the service is added to `mncs-language` and consumed here rather than reimplemented here. The service currently consumes one such upstream API beyond main's baseline: the [`NameResolutionIndex`](https://github.com/epi13/mncs-language/pull/…) recorded by elaboration (`mncs-compiler`), which provides authoritative use-site→declaration binding without duplicating scoping rules.

## Repository layout

```text
crates/
  service-core/     resident core (documents, snapshots, indexes, queries)
  lsp/              LSP adapter binary (tower-lsp)
  mcp/              MCP adapter binary (rmcp), read-only tools
tests/fixtures/     representative MNCS sources shared by all test levels
docs/               architecture, protocol model, agent interface, trust boundary
```

## Usage

Build and test everything:

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Run the LSP server over stdio against an MNCS workspace:

```bash
MNLS_WORKSPACE_ROOT=/path/to/mncs/workspace cargo run -p mncs-lsp
```

Any LSP-capable editor can attach; e.g. Neovim (built-in LSP):

```lua
vim.lsp.start({
  name = "mncs",
  cmd = { "mncs-lsp" },             -- from cargo build --release -p mncs-lsp
  root_dir = vim.fs.root(0, { ".git" }),
})
```

Run the MCP server over stdio:

```bash
MNLS_WORKSPACE_ROOT=/path/to/mncs/workspace cargo run -p mncs-mcp
```

Example Claude Code registration:

```bash
claude mcp add mncs -- MNLS_WORKSPACE_ROOT=/path/to/mncs/workspace mncs-mcp
```

Exercise an example workspace end-to-end (core-level behavior is exercised continuously by the test suite):

```bash
cargo run -p mncs-mcp <<< '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
```

or point either server at this repository's own fixtures:

```bash
MNLS_WORKSPACE_ROOT=$PWD/tests/fixtures cargo run -p mncs-lsp
```

Note on dependencies: while the upstream name-resolution PR is in review, this workspace pins `mncs-language` crates to its feature branch. After that PR merges, switch the `[workspace.dependencies]` entries to `branch = "main"` (or a released version).

## Core principles

1. **One semantic authority.** The service consumes `mncs-language`; it does not redefine MNCS.
2. **Persistent semantic state.** Repeated queries reuse resident workspace analysis instead of reconstructing the program.
3. **Identity-bound interaction.** Every response names the exact snapshot (source identity + generation) it was computed against.
4. **Human and machine symmetry.** Editors and agents inspect the same underlying semantic structure through role-appropriate representations.
5. **PASS / FAIL / UNKNOWN preservation.** Missing or bounded evidence is never converted into stronger claims.
6. **Bounded work.** Expensive verification, backend execution, Forge search, or Fabric work is never triggered by ordinary queries.
7. **Protocol independence.** Internal concepts are defined by the service query model, not by LSP/MCP schemas.
8. **Fail closed on stale state.** Ambiguity yields explicit `unsupported`/`unresolved` outcomes, not guesses.
9. **Semantic density for agents.** Structured responses favor identities, kinds, relationships, spans, contracts, capabilities, effects, and obligation state over prose blobs.

## Relationship to the MNCS family

- **MNCS Language** defines the language semantics and compiler-facing artifacts.
- **MNCS Language Service** keeps those semantics resident and queryable for humans and machines.
- **Forge** may consume semantic state, obligations, and candidate analysis but does not become the semantic authority.
- **RAVEL** may coordinate agents using identity-bound tasks and semantic context supplied by the service.
- **Fabric** may execute explicitly requested bounded work; it is not part of editor-time analysis.
- **Commons / Family Records** may persist durable results, but transient editor state should not automatically become durable family evidence.

## License

Apache-2.0.
