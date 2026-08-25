# `.mncs` adoption measurement — Linguist readiness

Measured: **2026-08-25**. Reproduce with `./census_mncs.sh`.

## Upstream acceptance rule (verified against `github-linguist/linguist@main`, CONTRIBUTING.md, 2026-08-25)

> - at least **2000 files per extension or filename** indexed in the last year
>   (the number you see at the top of the search results), excluding forks,
>   for extensions expected to occur more than once per repo, like Ruby's
>   `.rb` extension.
> - at least **200 files** … for extensions expected to only occur once per repo.
> - the results should show a **reasonable distribution across unique
>   `:user/:repo` combinations**, assessed by manually and randomly clicking
>   through the results.

`.mncs` is a multi-file-per-repo style source extension, so the applicable bar
is the 2000-file one, plus the distribution requirement.

## Measurement results, 2026-08-25

| Channel | Result | Reliability |
| --- | --- | --- |
| Known-family census (trees API) | **76 files** across **7 public repos**, **1 owner** | Exact for those repos |
| Legacy REST `search/code` `path:*.mncs` | 0 | Unusable — returns 0 even for accepted languages (e.g. `path:*.gleam`) |
| Logged-in code search (authoritative) | not scriptable; manual check required | This is what Linguists maintainers assess |

Family census detail (default branches):

| Repository | `.mncs` files |
| --- | --- |
| epi13/mncs-language | 31 |
| epi13/mncs-language-service | 11 |
| epi13/RAVEL | 5 |
| epi13/mncs-lineage | 4 |
| epi13/mncs-validator-rs | 5 |
| epi13/machine-native-complexity-standard | 5 |
| epi13/Machine-Native-Experimental-Learning | 15 |
| **Total** | **76** |

## Verdict

**The adoption requirement is not met — by a wide margin — and must not be
circumvented.**

1. Volume: ~76 known files vs ≥2000 required. Even granting unknown files
   beyond the family repos, there is no evidence of third-party usage at any
   scale.
2. Distribution: effectively all usage is concentrated in one owner's
   repositories (`epi13`). Linguist explicitly filters out dominant owners
   during assessment ("If particular users are showing a high proportion of
   the results, for example the primary language owner, we will filter out
   those users"). With `-user:epi13` applied, measured usage is zero.

## What would change the verdict

- MNCS being adopted by independent projects/users with real `.mncs` code on
  public GitHub.
- Rerun `./census_mncs.sh`; when the logged-in search shows ≥2000 indexed
  `.mncs` files in the last year excluding forks AND plausible distribution
  after filtering dominant owners, proceed to
  [`../PR-CHECKLIST.md`](../PR-CHECKLIST.md).

## Method notes

- Trees-API census counts blobs ending in `.mncs` on each repo's default
  branch (`HEAD`); history and non-default branches are excluded, matching
  how code search indexes content.
- The legacy REST code-search endpoint indexes a limited subset of
  repositories and demonstrably cannot detect even accepted rare languages;
  it is reported here only so future reruns do not mistake its zeros for
  evidence.

## Ecosystem finding: two artifact types share `.mncs`

During the 2026-08-25 sweep we found that `epi13/mncs-validator-rs` (and
`machine-native-complexity-standard`) use `.mncs` for **binary ZIP package
fixtures**, not source modules. Consequences:

1. **Linguist impact: none.** Linguist skips binary blobs during
   classification, so packaged `.mncs` files neither pollute language
   statistics nor trigger misclassification. Worth one sentence in the
   eventual PR body.
2. **Ecosystem concern (flagged to the language pass):** using `.mncs` for
   both source modules and package archives invites tooling confusion
   (editors will try to highlight zips; searches conflate them). Recommend
   the family standardize a distinct package extension (e.g. `.mncspkg`) or
   document the dual use authoritatively.
