# GitHub/Linguist integration kit

Everything needed to make MNCS a recognized GitHub language — prepared now,
submitted only when upstream's usage threshold is met.

## Contents

| Path | Purpose |
| --- | --- |
| `languages.yml.fragment` | Exact entry for upstream `lib/linguist/languages.yml` (schema-verified 2026-08; `language_id` deliberately omitted — `script/update-ids` mints it) |
| `samples/` + `PROVENANCE.md` | Curated real-world corpus with origin/revision/license per file |
| `PR-CHECKLIST.md` | Gate-by-gate runbook for the eventual upstream PR |
| `validate_linguist.sh` | Reproducible validation against a local linguist clone |
| `adoption/census_mncs.sh` | Rerunnable public-usage measurement |
| `adoption/MEASUREMENTS.md` | Dated, honest measurements and verdict |

## Current status: GitHub/Linguist-ready, upstream acceptance pending

Implemented locally: grammar packaged for Linguist consumption, language
metadata, samples, validation workflow, adoption tooling.

Blocked externally: Linguist requires ≥2000 indexed `.mncs` files/year
excluding forks **and** distribution across independent owners. Measured
2026-08: ~76 files in one owner's repositories. See
[`adoption/MEASUREMENTS.md`](adoption/MEASUREMENTS.md). No PR is to be
opened until that changes.

## Quick use

```bash
# measure readiness (rerunnable)
./adoption/census_mncs.sh

# validate the whole integration against a local linguist checkout
LINGUIST_DIR=$HOME/src/linguist ./validate_linguist.sh

# when adoption passes: follow PR-CHECKLIST.md gate by gate
```

## Why the grammar lives in this repository

Upstream's `script/add-grammar <repo-url>` vendors a *repository* as a
submodule under `vendor/grammars/`; many vendored sources are entire mixed
projects (whole VS Code extensions are precedent). Hosting
`integration/static-syntax/mncs.tmLanguage.json` inside this Apache-2.0
repository satisfies Linguist's license checks (`licensed` reads the repo
LICENSE) without inventing a separate single-purpose repo. If maintainers
ever prefer a dedicated grammar repository, the grammar file plus its README
move verbatim.
