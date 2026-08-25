# GitHub language support for MNCS

Status: **GitHub/Linguist-ready; upstream acceptance pending.** GitHub does
not officially recognize MNCS today. Everything under our control is
implemented and tested locally; the only remaining blocker is real-world
adoption, which only actual usage can satisfy.

---

## 1. What GitHub Linguist provides

[github-linguist/linguist](https://github.com/github-linguist/linguist) is
the library GitHub uses to decide *what a file is*. Landing MNCS there
yields, automatically:

- **repository language statistics** (the language bar and percentages);
- **file classification** for `.mncs` (diffs rendered as MNCS, not text);
- **syntax highlighting** of `.mncs` on github.com via TextMate-compatible
  grammars vendored by Linguist;
- **Markdown fenced-code identification**: ```` ```mncs ```` blocks map to
  `source.mncs` highlighting through the registered grammar;
- **language search/filtering** (`language:MNCS`), language color in the UI,
  repository "languages" badges, and Linguist-powered editor ecosystems
  downstream (many editors consume Linguist's grammar registry).

Explicitly **outside** Linguist's scope (see §11): CodeQL analysis,
advanced code navigation, and any semantic understanding of MNCS code.

## 2. What we have implemented locally

All inside this repository, under [`integration/`](../integration/) and
[`crates/static-syntax/`](../crates/static-syntax/):

| Asset | Location |
| --- | --- |
| Production TextMate grammar (`source.mncs`) | [`integration/static-syntax/mncs.tmLanguage.json`](../integration/static-syntax/mncs.tmLanguage.json) |
| Grammar validation + tokenization test engine | `crates/static-syntax` (`grammar.rs`, `tokenizer.rs`) |
| Real-document highlighting tests | `crates/static-syntax/tests/tokenization.rs` (25 tests over fixtures + curated samples) |
| Drift conformance vs the authoritative lexer | `crates/static-syntax/tests/conformance.rs` (8 live checks) |
| Prepared upstream language definition | [`integration/github-linguist/languages.yml.fragment`](../integration/github-linguist/languages.yml.fragment) |
| Curated sample corpus + provenance/licenses | [`integration/github-linguist/samples/`](../integration/github-linguist/samples/) |
| Upstream PR runbook | [`integration/github-linguist/PR-CHECKLIST.md`](../integration/github-linguist/PR-CHECKLIST.md) |
| Reproducible Linguist validation | [`integration/github-linguist/validate_linguist.sh`](../integration/github-linguist/validate_linguist.sh) |
| Adoption measurement tooling + dated results | [`integration/github-linguist/adoption/`](../integration/github-linguist/adoption/) |
| CI protection (assets, Rust, weekly drift watch) | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |

The grammar covers the current Source Profile 0.1–0.5 surface: header and
module declarations, function signatures (including multi-line parameter
lists), contracts/capabilities/effects (`requires`/`ensures`/`assumes`,
`capability`, `effect … authorized_by …`), statements, bounded iteration
(`iterate … up_to … carrying … next`), finite types with variants, records
(declarations, literals, functional update `..base`), field projection,
calls, boolean/integer/version literals, operators, punctuation, line and
block comments.

## 3. How the static grammar relates to mncs-language semantics

The project's constitutional rule applies here verbatim:

> `mncs-language` remains the sole syntactic and semantic authority.
> The TextMate grammar is a **presentation adapter** — nothing more.

Concretely, the grammar classifies lexical forms only. It does not resolve
names, infer types, check capabilities or contracts, or validate anything.
A file full of semantic errors still highlights perfectly — that is by
design, mirroring how every static grammar behaves on GitHub.

Two complementary paths exist, deliberately:

| Path | Consumer | Depth |
| --- | --- | --- |
| **Static grammar** (`source.mncs`) | GitHub, lightweight editors, renderers | lexical classification, zero setup |
| **LSP semantic tokens** (`mncs-lsp`) | capable editors and agents | authoritative resolution — identifiers classified by resolved meaning (function vs parameter vs variant vs field vs type), contracts/effects/obligation state |

Neither replaces the other. The static grammar must never grow semantics;
the LSP path is untouched by this work (see `crates/service-core/src/render.rs`
for the semantic-token implementation).

## 4. How grammar drift from mncs-language is prevented

`mncs-language` evolves quickly; a silently stale grammar would misrepresent
the language on GitHub. Four mechanical layers (detailed in
[`integration/static-syntax/README.md`](../integration/static-syntax/README.md)):

1. **Compile-time tripwire.** `scopes::expected_scope` maps every upstream
   `TokenKind` variant exhaustively. Add/remove/rename a variant upstream →
   this crate stops compiling.
2. **Live bidirectional conformance.** Every `cargo test` lexes a probe
   corpus with the *actual* `mncs-syntax` lexer (a git dependency on main)
   and asserts, per token: keywords upstream ⇒ keyword scopes here;
   keyword scopes here ⇒ reserved upstream; operators/punctuation/comments/
   literal classifications match; profile headers 0.1–0.5 all recognized;
   `.mncs` remains canonical.
3. **Manifest count invariant.** Distinct keyword kinds observed upstream
   must equal `KEYWORD_SPELLINGS` (24). Reclassification breaks it loudly.
4. **Weekly drift-watch CI.** A scheduled job refreshes the `mncs-syntax`
   dependency to latest main and reruns conformance, so upstream changes
   surface within days rather than at the next manual refresh.

Known unavoidable local copy: the reserved-word *spellings* (`KEYWORD_SPELLINGS`)
live inside the upstream lexer's private match today. Everything about them
— existence, kind pairing, count — is verified against the live lexer each
run; only a brand-new keyword's spelling itself requires a one-line manifest
addition, which layer 1 forces into view immediately because new keywords
always introduce new token kinds.

## 5. How to test the grammar

```bash
cargo test -p mncs-static-syntax            # tokenization + conformance suites
cargo test --workspace                      # everything, as CI runs it
```

To eyeball a file's tokenization:

```bash
cargo run -p mncs-static-syntax --example dump -- path/to/file.mncs
```

## 6. How to test against Linguist

```bash
git clone https://github.com/github-linguist/linguist "$HOME/src/linguist"
(cd "$HOME/src/linguist" && bundle install)
LINGUIST_DIR="$HOME/src/linguist" integration/github-linguist/validate_linguist.sh
```

The script injects the language definition, installs the samples, retrains
the classifier database, verifies every sample classifies as MNCS (aliases
and extension included), runs `github-linguist --breakdown` on a scratch
repo, and reverts its changes unless `KEEP=1`. Full details are documented
in the script header. It intentionally avoids Docker; the one step that
needs Docker (grammar submodule registration via upstream's own tooling) is
performed during the real PR per [`PR-CHECKLIST.md`](../integration/github-linguist/PR-CHECKLIST.md).

## 7. What the upstream submission requires

Current rules verified against `github-linguist/linguist@main`, 2026-08:

1. an entry in `lib/linguist/languages.yml` (`type`, `extensions`,
   `tm_scope`, `ace_mode` mandatory; our fragment also sets `color` and
   `aliases`; `language_id` comes later from `script/update-ids`);
2. a grammar hosted in a licensed public repository, added via
   `script/add-grammar <url>` (Docker-based; licenses accepted include
   Apache-2.0 — ours);
3. ≥2 representative real-world samples under `samples/<Language>/`
   ("Hello world" explicitly rejected); provenance and license stated;
4. a GitHub search link demonstrating in-the-wild usage;
5. heuristics only if another language claims the same extension (none does
   for `.mncs`).

The complete gate-by-gate runbook lives in
[`PR-CHECKLIST.md`](../integration/github-linguist/PR-CHECKLIST.md).

## 8. Current adoption/usage status

Measured **2026-08-25**, reproducible via
[`adoption/census_mncs.sh`](../integration/github-linguist/adoption/census_mncs.sh):

- known public `.mncs` files: **~76**, across **7 repositories**, all owned
  by **one account**;
- upstream bar: ≥2000 indexed files/year excluding forks **and** plausible
  distribution after filtering dominant owners;
- verdict: **requirement not met**; measured honestly, not gamed.

Full numbers, channel caveats (the legacy REST code search demonstrably
returns 0 even for accepted languages like Gleam, so logged-in search is the
only authoritative measure), and rerun instructions:
[`adoption/MEASUREMENTS.md`](../integration/github-linguist/adoption/MEASUREMENTS.md).

## 9. What remains blocked externally

Exactly one thing: **independent adoption**. Files written by people and
projects outside the founding organization, indexed on public GitHub, in
sufficient quantity and spread. No amount of engineering changes this; when
it happens, §7 becomes mechanical.

## 10. Exact steps when the day arrives

See [`PR-CHECKLIST.md`](../integration/github-linguist/PR-CHECKLIST.md).
Summary: rerun census → confirm assets fresh (`cargo update -p mncs-syntax`
+ full tests) → local linguist validation green → fork linguist, insert
fragment, `script/add-grammar https://github.com/epi13/mncs-language-service`,
copy samples, `script/update-ids`, `rake samples` → open PR with template
filled (search link, sample sources/licenses, grammar repo, color rationale).

## 11. GitHub capabilities outside Linguist's scope

Classified so nobody conflates them later:

| Capability | Class | Note |
| --- | --- | --- |
| Language stats, `.mncs` diffs, fenced-code highlighting, `language:MNCS` search | **automatic after Linguist acceptance** | covered by this work |
| CodeQL analysis of `.mncs` | **separate future project** | needs extractors/compilers against MNCS toolchain; unrelated to Linguist; do not bundle |
| GitHub code navigation / symbol indexing | separate future project | upstream stack assumes tree-sitter/LSP conventions; substantial independent work |
| GitHub App for MNCS services | rejected unless a concrete need appears | presence without function adds nothing |
| Editor marketplace extensions (VS Code/Zed/Sublime) | independently useful, later | reuse `integration/static-syntax/mncs.tmLanguage.json` directly; no parallel grammars |

## 12. Upstream asks for the next mncs-language pass

Small API additions that would strengthen layers above (recorded for the
language team; none block anything):

1. Export a keyword inventory, e.g. `pub const KEYWORDS: &[(&str, TokenKind)]`
   or `pub fn keyword_spelling(kind: TokenKind) -> Option<&'static str>` in
   `mncs-syntax`. This would delete the one remaining local copy
   (`KEYWORD_SPELLINGS`) and make drift detection fully derivation-based.
2. If/when source syntax grows constructs the lexer tokenizes but a plain
   regex grammar cannot represent faithfully (string literals with escapes
   would be the first), consider exposing a minimal `tokenize(text) ->
   Vec<(kind, range)>` convenience wrapper — already effectively public via
   `lex(&SourceEnvelope)`, just friendlier.
3. When module imports (`use`) land, announce them in release notes so the
   grammar pass adds the corresponding rules the same week (the grammar's
   structure makes this a ten-line change).
