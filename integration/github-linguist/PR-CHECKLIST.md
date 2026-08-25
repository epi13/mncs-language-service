# Upstream Linguist PR checklist — MNCS

Goal: when MNCS satisfies GitHub's usage threshold, submitting
[`github-linguist/linguist`](https://github.com/github-linguist/linguist)
support should take under an hour with zero engineering invention. This file
is that runbook. **Do not open the PR before the adoption gate passes** —
upstream policy closes premature language PRs on sight.

---

## Gate 0 — adoption requirement (hard precondition)

Verified upstream rule (`CONTRIBUTING.md`, 2026-08): ≥2000 indexed
`.mncs` files in the last year excluding forks (multi-file extension bar),
plus reasonable distribution across unique `:user/:repo` after filtering
dominant owners.

- [ ] Rerun `adoption/census_mncs.sh`; record output in
      `adoption/MEASUREMENTS.md`.
- [ ] Open <https://github.com/search?type=code&q=NOT+is%3Afork+path%3A*.mncs>
      while logged in; confirm the result count ≥2000 **and** spot-check at
      least three pages of results for owner diversity (Linguist maintainers
      filter `-user:<owner>` for dominant accounts).
- [ ] If either fails: stop. Everything else here stays valid; revisit later.

## Gate 1 — assets are current

- [ ] `integration/static-syntax/mncs.tmLanguage.json` matches today's
      authoritative lexer: `cargo test --workspace` green (the conformance
      suite runs the live `mncs-syntax` lexer).
- [ ] Refresh the dependency first so the check is meaningful:
      `cargo update -p mncs-syntax && cargo test --workspace`.
- [ ] `languages.yml.fragment` still matches upstream's schema: diff a field
      sample of `lib/linguist/languages.yml` against this fragment's fields
      (`type`, `color`, `aliases`, `extensions`, `tm_scope`, `ace_mode`;
      no `language_id`).
- [ ] Samples in `samples/` still exist at their origin URLs and licenses
      unchanged; `PROVENANCE.md` updated with fresh revision hashes if the
      origins moved.

## Gate 2 — local validation

- [ ] With Ruby+Bundler available:
      `LINGUIST_DIR=/path/to/fresh/linguist ./validate_linguist.sh`
      (fresh clone of upstream main). All five steps pass; every sample
      classifies as MNCS; `github-linguist --breakdown` reports MNCS.
- [ ] Optional deeper pass: `RUN_FULL_TESTS=1` to run upstream's whole suite.

## Gate 3 — assemble the change

1. Fork linguist; branch e.g. `add-mncs`.
2. Insert `languages.yml.fragment` body into `lib/linguist/languages.yml`
   between `MLIR` and `MQL4` (alphabetical, case-sensitive; re-verify
   position against current main).
3. Register the grammar exactly as upstream requires (needs Docker):
   ```bash
   script/add-grammar https://github.com/epi13/mncs-language-service
   ```
   This vendors this repository as `vendor/grammars/mncs-language-service`
   and compiles `source.mncs` from
   `integration/static-syntax/mncs.tmLanguage.json`. Fix any grammar
   compiler complaints **in this repository**, not in the fork.
4. Copy `samples/*.mncs` into `samples/MNCS/`.
5. Run `script/update-ids` to mint the permanent `language_id`; commit its
   output. Never invent an id by hand.
6. `bundle exec rake samples` regenerates the classifier database; commit it.

## Gate 4 — the pull request

Upstream rejects PRs whose template is unfilled. Fill it as follows:

- [ ] Section "I am adding a new language".
- [ ] Search results link (logged-in count):
      `https://github.com/search?type=code&q=NOT+is%3Afork+path%3A*.mncs`
- [ ] Sample sources: per-file links from [`samples/PROVENANCE.md`](samples/PROVENANCE.md)
      (each already records origin repository, path, revision).
- [ ] Sample licenses: Apache-2.0 throughout (state it explicitly; link
      PROVENANCE.md).
- [ ] Grammar repo URL: <https://github.com/epi13/mncs-language-service>
      (Apache-2.0 — on Linguist's allowed license list).
- [ ] Color: confirm `#0e7c7b` or replace. Rationale template: "Teal,
      provisional pick reflecting the project's status/evidence palette;
      chosen by the language author." Colors are sticky — get it right once.
- [ ] Heuristics section: `.mncs` has no conflicting claimant in
      `languages.yml` (verified 2026-08); state that no heuristic is needed,
      linking the empty search for other users of the extension.
- [ ] Link PROVENANCE.md (or its gist content) directly in the PR body —
      reviewers ask for provenance every time.

## After merge

- GitHub picks up language stats and highlighting on the next Linguist
  release; diffs/search filters follow. Nothing to do in this repo except:
- [ ] Flip `docs/github-language-support.md` wording from "GitHub/Linguist-ready"
      to "recognized by GitHub" with the merged PR link.
- [ ] Update ROADMAP entry.

## Non-goals (do not bundle into the PR)

- CodeQL support — separate compiler/toolchain effort, unrelated to Linguist.
- Editor marketplace extensions (VS Code/Zed) — consume the same grammar
  from `integration/static-syntax/`; separate projects, later.
