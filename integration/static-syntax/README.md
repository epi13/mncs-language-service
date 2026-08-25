# MNCS static syntax package

`mncs.tmLanguage.json` is the canonical TextMate-compatible grammar for
`.mncs` sources, scope `source.mncs`. It is a **presentation adapter**: it
classifies lexical forms for editors and GitHub-style highlighting. It does
not — and must not — resolve names, check capabilities, validate contracts,
or otherwise reinterpret MNCS semantics; that remains the exclusive domain of
[`mncs-language`](https://github.com/epi13/mncs-language).

## What it covers (current Source Profile 0.1–0.5 surface)

- header (`mncs 0.5;`) and module declarations, including `mncs.core.*`
  standard-library paths;
- `fn` signatures with multi-line parameter lists;
- contracts (`requires`/`ensures`/`assumes <name>`), `capability`, and
  `effect … authorized_by …` clauses;
- statements: `let x: T = e;`, `if/else`, `fail`, `return`;
- bounded iteration: `iterate i up_to N carrying s: T = init { next s = e; }`;
- finite types (`enum Name { V1, V2 }`) with variant references in `match`
  arms and `Type.VARIANT` access;
- records: declarations, field entries, record literals with functional
  update (`Pair { ..base, left: 1 }`);
- expressions: calls, field projection, boolean literals, integer literals,
  version literals, comparison/arithmetic/assignment operators;
- line comments and block comments.

## Scope scheme

Conventional TextMate top-level families so common themes work unchanged:

| Construct | Scope |
| --- | --- |
| header keyword / module | `keyword.other.header.mncs`, `keyword.declaration.module.mncs` |
| declaration keywords (`fn let enum record effect capability`) | `keyword.declaration.*.mncs` |
| control flow (`if else return fail match while next`) | `keyword.control.flow.*.mncs` |
| iteration (`iterate up_to carrying`) | `keyword.control.iteration.*.mncs` |
| contract clauses | `keyword.control.contract.{requires,ensures,assumes}.mncs` + `entity.name.contract.mncs` |
| effects/capabilities | `keyword.declaration.effect.mncs`, `keyword.control.authorized-by.mncs`, `entity.name.{effect,capability}.mncs` |
| names | `entity.name.function|enum|record|variant|namespace|type|contract.mncs` |
| bindings/params/members | `variable.parameter.mncs`, `variable.name.mncs`, `variable.other.member.mncs`, `variable.function.mncs` (calls), `variable.other.property.mncs` |
| literals | `constant.numeric.integer|version.mncs`, `constant.language.boolean.mncs` |
| comments | `comment.line.double-slash.mncs`, `comment.block.mncs` (+ `punctuation.definition.comment.mncs`) |
| operators | `keyword.operator.{comparison,arithmetic,assignment,arrow,spread}.mncs` |
| punctuation | `punctuation.separator.colon/comma.mncs`, `punctuation.terminator.statement.mncs`, `punctuation.accessor.dot.mncs`, `punctuation.section.{block,parentheses}.*.mncs` |

Bare identifiers are deliberately unscoped — themes style declarations,
calls, members, and keywords, not every word. Richer identifier
classification (resolved function vs variable vs variant) is the LSP's job.

## Known, deliberate limitations

Documented rather than hidden:

1. **Nested block comments to depth 8.** The authoritative lexer counts
   nesting arbitrarily deep; a static grammar cannot recurse without bound.
   The grammar chains eight nested region rules, which covers all real code.
   Beyond depth eight, inner content is still consumed as comment text but
   delimiters stop receiving distinct scopes. Degradation is conservative:
   text stays commented; it never flips back to code early.
2. **Profile-relative reserved words.** The lexer/parser demote `record`
   (pre-0.5) and iteration words (pre-0.4) to ordinary identifiers by source
   profile. A stateless static grammar highlights them as keywords
   everywhere, matching the *lexer's* classification (which is also
   profile-independent). Old-profile files that use these words as variable
   names will show keyword coloring there.
3. **Module-path dots** are folded into one `entity.name.namespace.mncs`
   token rather than scoped as individual accessors.
4. **Malformed sources highlight conservatively.** Keywords, comments, and
   literals remain classified inside broken code; unterminated regions run
   to end-of-line/file without inventing structure.

## Portability subset

The grammar is authored to a subset accepted identically by Oniguruma-based
engines (VS Code, GitHub), PCRE (Linguist's compiler), and the Rust test
engine: character classes incl. `\p{…}`, alternation, quantifiers,
lookaheads only (no lookbehind, no backreferences-in-end, no `\G`, no
`while` rules, no capture-patterns, no injections). `crates/static-syntax/src/grammar.rs`
enforces this subset mechanically — new constructs fail validation loudly
instead of silently diverging between engines.

## How it is tested

All tests live in [`crates/static-syntax/tests/`](../../crates/static-syntax/tests/):

- `tokenization.rs` — real documents (this repository's fixtures plus the
  curated Linguist samples) run through a genuine begin/end/include stack
  engine asserting concrete scopes per construct, including malformed
  sources;
- `conformance.rs` — drift checks against the live authoritative lexer
  (see below).

## Keeping it synchronized with mncs-language

The grammar must never drift from the compiler. Four layers:

1. **Compile-time tripwire** — `scopes::expected_scope` matches
   exhaustively over upstream `TokenKind`; adding or removing a token kind
   upstream breaks this crate's build.
2. **Live bidirectional conformance** — every test run lexes a probe corpus
   with the actual `mncs-syntax` lexer and asserts: every manifest spelling
   still lexes as its kind; every keyword-kind token is in the manifest;
   every classification mirrors the grammar; identifiers never get keyword
   scopes.
3. **Manifest count invariant** — distinct keyword kinds observed upstream
   must equal the manifest size (24 today).
4. **CI lockfile refresh** — scheduled workflow updates the `mncs-syntax`
   git dependency and reruns the suite, so upstream changes surface within
   days, not months.

When (not if) the language evolves, the fix path is always: update
`KEYWORD_SPELLINGS`/scope mapping if needed → adjust grammar rules → tests
green. The upstream ask that would simplify layer 2 further (an exported
keyword inventory API) is recorded in
[`docs/github-language-support.md`](../../docs/github-language-support.md).

## License

Apache-2.0 (repository `LICENSE`). This file is intentionally hosted in this
licensed repository so Linguist's `licensed` tooling can clear it as a
grammar submodule directly.
