# MNCS Language Service

`mncs-language-service` is the resident semantic service for the MNCS language.

It exposes the same authoritative MNCS language semantics to editors, coding agents, and other MNCS components through multiple protocol adapters (LSP and MCP today), while keeping language semantics owned by [`mncs-language`](https://github.com/epi13/mncs-language).

The service maintains **resident workspace state**: it tracks documents, runs the authoritative `mncs-language` frontend once per content state, binds the results into identity-bound analysis snapshots, indexes them for navigation and semantic inspection, and answers repeated queries without recomputing anything.

## Status

**Resident workspace/event infrastructure and second-wave editor intelligence
are implemented and exercised.**

```text
MNCS source
   ↓
resident authoritative analysis   (mncs-syntax → mncs-compiler → mncs-model)
   ↓
identity-bound snapshot           (mncs:source:artifact:<sha256> + workspace generation)
   ↓
shared semantic query core        (mncs-service-core)
   ├── MNCS-native bounded query kernel (real mncs-language compilation/execution)
   ├── LSP  → mncs-lsp            editor diagnostics/navigation/hover/tokens/completion
   └── MCP  → mncs-mcp            agent semantic inspection (read-only)
```

For shared local development, start one resident host and point every adapter
at its Unix socket:

```bash
MNLS_WORKSPACE_ROOT=/path/to/workspace \
MNLS_SERVICE_SOCKET=/path/to/workspace/.mncs/mnls-language-service.sock \
cargo run -p mncs-service-core --bin mnls-language-service-host
```

Set the same `MNLS_SERVICE_SOCKET` for `mncs-lsp`, `mncs-mcp`, and machine
clients. They then share one workspace generation counter, exact source
identities, semantic snapshots, and bounded `mncs.workspace-change/2` events
whose cursors carry an explicit stream identity. The LSP and MCP implementations remain thin protocol adapters over
the `LanguageServiceClient` boundary; an in-process service is retained for
tests and compatibility when no socket is configured.

What works today:

- document lifecycle (open/change/save/close) with unsaved editor buffers overriding disk;
- **incremental synchronization**: ranged `didChange` edits apply against
  buffer state in order (UTF-16 aware), alongside full-document replacement;
- authoritative parsing/elaboration/validation through `ReferenceCompiler::front_end`;
- immutable snapshots bound to exact source identities with correct coarse invalidation;
- structured diagnostics preserving codes/stages/severities/spans, plus
  causal `related` entries projected to their owning dependency locations
  (URI, and exact range when the dependency is resident) instead of
  misattributed same-file ranges;
- hover, go-to-definition, **go-to-declaration**, **go-to-type-definition**,
  references, highlights — all from authoritative name resolution, never text search;
- **signature help** (token-driven, so it keeps answering mid-typing when no
  AST exists), with active-argument tracking and generic parameters;
- **semantic rename** with workspace-wide bound-reference collection,
  lexical validation, same-scope collision refusal, and explicit errors
  (never silent no-ops);
- **call hierarchy** (prepare/incoming/outgoing) from authoritative
  resolutions with call-site ranges, workspace-wide;
- **selection ranges** from CST ancestry, **inlay hints** (parameter names
  for arity-matching resolved calls), **document and range formatting**
  (deterministic, idempotent, token-preserving);
- **code actions**: missing-import quickfixes for unresolvable calls
  (`MNE131`) when another module exports the name;
- document/workspace symbols including Source Profile 0.5 record types and fields;
- semantic tokens, conservative completion, folding ranges;
- call-graph dependencies/dependents derived from elaborated bodies;
- obligations with preserved `PASS` / `FAIL` / `UNKNOWN` status;
- `debug_source_binding`, a shared source/debug projection that reuses
  compiler-owned module, function, first-class-test, source-span, and source
  identity fields; execution-operation identities join the exact
  `mncs.execution-source-map/1` operation span while failure-location and
  live-breakpoint support remain honest capability states;
- experimental `native_obligations`: projects the authoritative obligation
  statuses into a bounded MNCS query, executes the real
  `mncs-research-bytecode` backend, validates identity-bound results, and
  differentially compares them with the Rust control result;
- experimental `native_kind_count` (second native kernel): projects the
  symbol index to stable kind tags and counts the wanted kind through the
  authoritative generic `mncs.core.sequences.v1::count<8>`, differentially
  compared with the Rust control (MCP tool included);
- candidate analysis (Phase 4): isolated candidate snapshots with language-owned
  semantic/obligation deltas and stale-evidence detection (`analyze_candidate`,
  MCP + native), never mutating the workspace baseline. Candidates elaborate
  against the same resident resolution as their baseline (workspace documents
  plus `MNCS_LIBRARY_PATH` standard-library roots), so editing an importing
  module yields real deltas instead of false unresolvable-import diagnostics;
- a read-only MCP tool surface for agents over the same resident state;
- `family_agent_context`, the bounded family preflight that composes the
  repository-local manifest, current language/compiler identities, Commons
  architecture and pressure projections, and optional non-normative Atlas
  orientation without creating a second family registry;
- `language_capabilities`, a bounded query over the authoritative
  `mncs-language/docs/language-capabilities.json` index. It provides a compact
  capsule, topic/symbol expansion, profile deltas, and source provenance so
  agents do not rediscover language facts by scanning the repository;
- **static syntax + GitHub/Linguist readiness (Phase 4.5)**: a production
  TextMate grammar (`source.mncs`) with mechanical drift protection against
  the authoritative lexer, plus prepared Linguist language metadata, licensed
  samples, validation tooling, and an honest adoption measurement. GitHub does
  not yet recognize MNCS — upstream acceptance is pending real-world usage
  ([details](docs/github-language-support.md));
- a structured **language-pressure ledger** ([`pressure/`](pressure/README.md))
  recording deficiencies MNCS itself exposed during service construction,
  each with reproducer, classification, and workaround cost.

What is explicitly not implemented yet: mutation/semantic patches beyond
rename (Phase 5+), fine-grained incremental invalidation and cancellation
(see [`pressure/LS-P-001.md`](pressure/LS-P-001.md) and
[`pressure/LS-P-004.md`](pressure/LS-P-004.md)), on-type formatting, type
hierarchy (MNCS has no inheritance to expose), and direct Forge/Fabric
execution integration. Both native kernels are bounded (eight slots),
select only the research-bytecode backend, and require `MNCS_LIBRARY_PATH`
for their authoritative standard-library modules. The service does include
a drift-guard fixture that resolves the shared MNCS-native Forge source
spine through `MNCS_LIBRARY_PATH`.

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

Workspace edits from LSP buffers and filesystem refreshes converge into the
same monotonic generation model. Event consumers receive compact diagnostic,
obligation, subject, impact, and limitation deltas rather than routine source
or compiler-output dumps. Event history is bounded and cursor-based; a compact
workspace checkpoint reconciles source identities after host restart, while
history aging still produces an explicit reset/UNKNOWN condition.

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
- static (TextMate) syntax presentation and third-party forge integration
  assets, as shallow presentation adapters over the authoritative language
  ([`integration/`](integration/README.md));
- interaction policy around stale snapshots and candidate changes;
- the MNCS-native query adapter source and its fail-closed differential policy;
- service observability and lifecycle;
- the shared `mncs.debug-source-binding/1` source projection consumed by
  `mncs-debug` and exposed through MCP without claiming live breakpoint
  execution or runtime suspension support.

A language semantic capability required by the service is added to `mncs-language` and consumed here rather than reimplemented here. The service currently consumes one such upstream API beyond main's baseline: the [`NameResolutionIndex`](https://github.com/epi13/mncs-language/pull/…) recorded by elaboration (`mncs-compiler`), which provides authoritative use-site→declaration binding without duplicating scoping rules.

The language capability index follows the same boundary: compiler/library facts
and curated guidance are owned by `mncs-language`; this service is only the
bounded query projection. `MNCS_LANGUAGE_CAPABILITY_INDEX` and
`MNCS_LANGUAGE_ROOT` are explicit discovery overrides for isolated checkouts.

The module resolver also validates discovered source declarations against their
requested import names (including version-tail compatibility). This keeps the
service bound to the declared module identity when multiple sibling language
and Forge roots contain similarly named files. The
`native-forge-service.mncs` fixture resolves `mncs.forge.core.v1` from the
shared Forge checkout and is a synchronization guard, not a second Forge
semantic implementation.

## Repository layout

```text
crates/
  service-core/     resident core (documents, snapshots, indexes, queries)
  static-syntax/    TextMate grammar validation, tokenization tests,
                    and live conformance against the mncs-language lexer
  lsp/              LSP adapter binary (tower-lsp)
  mcp/              MCP adapter binary (rmcp), read-only tools
integration/        third-party integration assets (TextMate grammar package,
                    KDE/KWrite/Kate and VS Code adapters, GitHub/Linguist
                    readiness kit) — see integration/README.md
tests/fixtures/     representative MNCS sources shared by all test levels
mncs/               service-specific MNCS query modules executed through
                    the authoritative compiler/backend
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

For a stable installed command, install the LSP binary from this repository:

```bash
cargo install --path crates/lsp --locked --bin mncs-lsp --root "$HOME/.local"
```

The installed executable is `mncs-lsp`. It speaks standard Language Server
Protocol over stdin/stdout; diagnostics and logs are sent through the LSP
transport or client logging channel, never as ad-hoc stdout text. Clients may
provide the workspace root during `initialize`; `MNLS_WORKSPACE_ROOT` is an
optional fallback for clients that do not send one.

### Fedora editor integration

The repository includes reproducible editor assets under
[`integration/`](integration/README.md). The canonical static grammar is
shared with the KDE and VS Code adapters; the resident LSP remains the source
of semantic answers.

Install the current-user KDE syntax definition for KWrite and Kate:

```bash
integration/kde/install.sh
```

KWrite gets syntax highlighting through KSyntaxHighlighting. Kate gets the
same highlighting plus LSP features when its LSP Client plugin is enabled;
copy [`integration/kate/mncs.kateproject.example`](integration/kate/mncs.kateproject.example)
to a project as `.kateproject` and ensure
`mncs-lsp` is on PATH. The project file uses the executable name rather than a
machine-specific path, so the Cargo-installed binary at
`$HOME/.local/bin/mncs-lsp` works when that directory is in PATH.

Package and install the thin VS Code adapter:

```bash
integration/vscode/install.sh
```

The extension contributes `*.mncs` syntax presentation and starts the real
`mncs-lsp` process. Set `mncs.languageServer.command` or `MNCS_LSP` only when
the executable is outside the resolver's standard user-local locations.

The current authoritative lexer has no string-literal token, so the bundled
grammars intentionally do not invent quoted-string highlighting. The service
advertises incremental synchronization, diagnostics (with related
locations), hover, definition, declaration, type definition, references,
document/workspace symbols, semantic tokens, completion, highlights,
folding, signature help, rename, formatting (document + range), selection
ranges, call hierarchy, inlay hints, and code actions (missing-import
quickfixes).

Semantic tokens intentionally stay within the standard editor vocabulary:
resolved functions, parameters, variables, types, enum members, properties,
namespaces, keywords, and numbers. Contracts, effects, capabilities,
assumptions, evidence, obligations, and verification states remain keyword /
identifier presentation or structured diagnostics and obligation responses;
they are not forced into misleading custom token types.

### OpenCode

OpenCode enables its built-in language servers and custom servers through the
`lsp` configuration object. Add the MNCS entry to the applicable global or
project `opencode.json`/`opencode.jsonc`, preserving any existing settings:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "mncs": {
      "command": ["mncs-lsp"],
      "extensions": [".mncs"]
    }
  }
}
```

The same configuration keeps ordinary built-in servers such as
`rust-analyzer` available. OpenCode's agent-facing `lsp` tool is experimental
in versions that expose it; enable it persistently in the user's environment
with `OPENCODE_EXPERIMENTAL_LSP_TOOL=true` (or the broader
`OPENCODE_EXPERIMENTAL=true`). A copyable configuration example is in
[`integration/opencode/opencode.jsonc`](integration/opencode/opencode.jsonc).

OpenCode starts `mncs-lsp` for `.mncs` files and uses the same semantic core as
other editors. The service supports incremental synchronization, published
diagnostics, hover, cross-file definition/declaration/references,
document/workspace symbols, semantic tokens, conservative completion,
highlights, folding, signature help, rename, formatting, selection ranges,
call hierarchy, inlay hints, and import-assist code actions. Fine-grained
incremental invalidations remain intentionally coarse until the authoritative
language APIs support them (see [`pressure/LS-P-001.md`](pressure/LS-P-001.md)).

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

Run the protocol and real-stdio tests directly:

```bash
cargo test -p mncs-lsp --test lsp_protocol
cargo test -p mncs-service-core --test module_imports
```

`mncs-language` is consumed from `main`, currently pinned at
`85051d2a` (post ingest/type-architecture/ABI-transport tranches, including
`SourceDiagnostic.related` leaf diagnostics and generic entrypoint support).
The authoritative `NameResolutionIndex` recorded by elaboration and the
public `contract_id` constructor are part of main.

Both experimental native MCP operations (`native_obligations`,
`native_kind_count`) additionally require `MNCS_LIBRARY_PATH` to point at a
checkout's `mncs-language/library` directory. They are read-only
differential proving paths: the Rust service still acquires the
authoritative data and retains it beside the MNCS result, while the bounded
aggregation executes from [`mncs/status_query.mncs`](mncs/status_query.mncs)
and [`mncs/filter_query.mncs`](mncs/filter_query.mncs).

## Core principles

1. **One semantic authority.** The service consumes `mncs-language`; it does not redefine MNCS.
2. **Persistent semantic state.** Repeated queries reuse resident workspace analysis instead of reconstructing the program.
3. **Identity-bound interaction.** Every response names the exact snapshot (source identity + generation) it was computed against.
4. **Human and machine symmetry.** Editors and agents inspect the same underlying semantic structure through role-appropriate representations.
5. **PASS / FAIL / UNKNOWN preservation.** Missing or bounded evidence is never converted into stronger claims.
6. **Fail-closed native execution.** A missing library, invalid artifact,
   malformed return value, unsupported backend, or Rust/MNCS mismatch yields an
   explicit unsupported result rather than a guessed semantic answer.
7. **Bounded work.** Expensive verification, backend execution, Forge search, or Fabric work is never triggered by ordinary queries.
8. **Protocol independence.** Internal concepts are defined by the service query model, not by LSP/MCP schemas.
9. **Fail closed on stale state.** Ambiguity yields explicit `unsupported`/`unresolved` outcomes, not guesses.
10. **Semantic density for agents.** Structured responses favor identities, kinds, relationships, spans, contracts, capabilities, effects, and obligation state over prose blobs.

## Relationship to the MNCS family

- **MNCS Language** defines the language semantics and compiler-facing artifacts.
- **MNCS Language Service** keeps those semantics resident and queryable for humans and machines.
- **Forge** may consume semantic state, obligations, and candidate analysis but does not become the semantic authority.
- **RAVEL** may coordinate agents using identity-bound tasks and semantic context supplied by the service.
- **Fabric** may execute explicitly requested bounded work; it is not part of editor-time analysis.
- **Commons / Family Records** may persist durable results, but transient editor state should not automatically become durable family evidence.

## License

Apache-2.0.
