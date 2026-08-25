# Sample provenance

Every file in this directory is real, in-use MNCS source copied verbatim from
a public Apache-2.0 repository in the MNCS family. This manifest records
origin, revision, and license for each so an eventual Linguist PR can state
provenance exactly as upstream requires ("Please state clearly the license
covering the code… link to the original source if possible").

All samples were captured on 2026-08-25.

| Sample | Origin | Revision | License |
| --- | --- | --- | --- |
| `flagship.mncs` | [epi13/mncs-language](https://github.com/epi13/mncs-language) → `examples/source/flagship.mncs` | `54886be810f3f9beea534bb3b29db9638be28881` (branch `main`) | Apache-2.0 |
| `profile05-record-values.mncs` | [epi13/mncs-language](https://github.com/epi13/mncs-language) → `examples/source/profile05-record-values.mncs` | `54886be810f3f9beea534bb3b29db9638be28881` (branch `main`) | Apache-2.0 |
| `cre3-retry-authority.mncs` | [epi13/mncs-language](https://github.com/epi13/mncs-language) → `examples/source/cre3-retry-authority.mncs` | `54886be810f3f9beea534bb3b29db9638be28881` (branch `main`) | Apache-2.0 |
| `core-status.mncs` | [epi13/mncs-language](https://github.com/epi13/mncs-language) → `library/core/status.mncs` | `54886be810f3f9beea534bb3b29db9638be28881` (branch `main`) | Apache-2.0 |
| `core-ordering.mncs` | [epi13/mncs-language](https://github.com/epi13/mncs-language) → `library/core/ordering.mncs` | `54886be810f3f9beea534bb3b29db9638be28881` (branch `main`) | Apache-2.0 |
| `ravel-core.mncs` | [epi13/RAVEL](https://github.com/epi13/RAVEL) → `mncs/workspace/ravel_core.mncs` | `32b6bc41e38767106e25609d31e83313d71b61dc` | Apache-2.0 |
| `synthetic-lineage-g0.mncs` | [epi13/mncs-language-service](https://github.com/epi13/mncs-language-service) → `tests/fixtures/lineage/synthetic-lineage-g0.mncs` | this repository | Apache-2.0 |

## Selection criteria

The corpus deliberately covers materially different language surfaces rather
than padding volume:

- **Profile spread** — Source Profiles 0.2 (`flagship`), 0.4-era authority
  constructs (`cre3-retry-authority`), and 0.5 records (`profile05-record-values`,
  `synthetic-lineage-g0`);
- **Standard library** — `core-status`, `core-ordering` are authoritative
  `mncs.core.*` modules;
- **Ecosystem diversity** — RAVEL's reasoning core exercises nested `match`
  arms and record parameters beyond the language repo's own style;
- **Construct coverage** — contracts (`requires`/`ensures`), capabilities,
  effects (`effect … authorized_by …`), bounded iteration
  (`iterate … up_to … carrying … next`), finite types and variants, record
  literals with functional update (`..base`), field projection, boolean and
  integer literals, line comments, multi-line signatures.

## Honest note on volume

This corpus exists to make highlighting tests representative and to seed the
future Linguist `samples/` submission. It does **not** satisfy Linguist's
adoption requirement (≥2000 indexed `.mncs` files across distributed public
repositories within the last year); see `adoption/MEASUREMENTS.md`. No sample
was manufactured to game that threshold.
