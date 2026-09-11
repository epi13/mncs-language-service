# Third-party integration assets

This directory is the home for everything that lets the *outside world*
consume MNCS source: editors, forges, documentation renderers, search
infrastructure.

## Ownership boundary (project law)

> [`mncs-language`](https://github.com/epi13/mncs-language) is the sole
> syntactic and semantic authority. Nothing here may redefine MNCS syntax or
> semantics; every asset in this directory is a **presentation or discovery
> adapter** over what `mncs-language` already defines.

`mncs-language-service` owns these adapters because they serve editors and
agents — the same clients the resident service supports — but they are kept
strictly shallow:

| Asset | Depth limit |
| --- | --- |
| TextMate grammar (`static-syntax/`) | Lexical classification only. No name resolution, no capability checking, no contract validation, no elaboration. |
| KDE syntax (`kde/`) | Lexical classification only, expressed through KSyntaxHighlighting semantic default styles. |
| Kate project (`kate/`) | Process wiring only. Kate asks `mncs-lsp` for all semantic behavior. |
| VS Code adapter (`vscode/`) | Process wiring plus the shared TextMate grammar. VS Code asks `mncs-lsp` for all semantic behavior. |
| Linguist metadata (`github-linguist/`) | File-type identification and language statistics only. |

The richer, authoritative path for capable environments remains LSP semantic
tokens from the resident service (`mncs-lsp`), which classify identifiers by
*resolved meaning* — function vs parameter vs variant vs field — using
authoritative analysis. The static grammar cannot and must not attempt that.

Drift between the static view and the authoritative lexer is prevented
mechanically; see `crates/static-syntax/` and
[`docs/github-language-support.md`](../docs/github-language-support.md).

## Layout

```text
integration/
  static-syntax/            canonical TextMate grammar package (source.mncs)
    mncs.tmLanguage.json    THE grammar — single source of truth for static highlighting
  kde/                      KSyntaxHighlighting XML for KWrite/Kate + validation
  kate/                     Kate LSP project configuration example
  vscode/                   thin VS Code grammar + LSP client extension
  github-linguist/          GitHub/Linguist readiness kit
    languages.yml.fragment  prepared upstream language definition
    samples/                curated real-world corpus + PROVENANCE.md
    PR-CHECKLIST.md         runbook for the eventual upstream PR
    validate_linguist.sh    reproducible validation against a local linguist checkout
    adoption/               usage measurement tooling + dated measurements
```

## Reuse policy

The grammar file itself is editor-agnostic:

- **GitHub/Linguist** — vendored via `script/add-grammar` (see PR-CHECKLIST).
- **KWrite / Kate** — `kde/mncs.xml` is a generated-by-hand presentation
  adapter validated against the same keyword manifest. Install it with
  `kde/install.sh`.
- **Kate LSP** — `kate/mncs.kateproject.example` configures the thin
  `mncs-lsp` adapter for a project.
- **VS Code** — `vscode/package.sh` packages an extension that copies this
  grammar into the VSIX and starts `mncs-lsp` through the standard client.
  Do not fork the rules.
- **Zed / Sublime / TextMate-compatible tools** — consume the same JSON
  (Sublime via conversion of this file, not a parallel hand-written syntax).
- **Documentation renderers** — any TextMate-compatible highlighter can load
  it.

If a target someday genuinely requires a different format, generate it from
this file mechanically rather than maintaining a second grammar.
