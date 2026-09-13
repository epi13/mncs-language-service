# MNCS Language-Service Pressure Ledger

Structured language-pressure records produced by building the language
service as a real-world MNCS consumer. Each record follows the campaign
quality standard: reasonable service requirement, natural MNCS expression,
preventer, minimal reproducer, expected vs actual behavior, classification,
workaround, and workaround cost.

## Status vocabulary

| Status | Meaning |
| --- | --- |
| OPEN | Reported with reproducer, not yet confirmed. |
| CONFIRMED | Reproduced against current `mncs-language`; root cause classified. |
| WORKAROUND | Service ships a compatibility boundary; deficiency remains upstream. |
| FIXED-UPSTREAM | Repaired in `mncs-language`; service still carries the workaround. |
| VERIFIED | Workaround removed; native implementation proven by tests. |
| REJECTED-AS-PRESSURE | Investigated and judged not a deficiency (with reasoning). |

## Records

| ID | Title | Status |
| --- | --- | --- |
| [LS-P-001](LS-P-001.md) | No incremental or recovery parsing; full frontend rerun per keystroke | WORKAROUND |
| [LS-P-002](LS-P-002.md) | No string literals: native text queries inexpressible in MNCS | CONFIRMED |
| [LS-P-003](LS-P-003.md) | Import-wrapped leaf diagnostics lack machine-readable provenance | CONFIRMED |
| [LS-P-004](LS-P-004.md) | No cancellation: analysis is synchronous and uninterruptible | CONFIRMED |
| [LS-P-005](LS-P-005.md) | JSON-RPC/LSP framing must remain a Rust boundary | REJECTED-AS-PRESSURE |
| [LS-P-006](LS-P-006.md) | Bounded symbol filtering via generic stdlib execution | VERIFIED |

## Lifecycle rules

- Do not mark VERIFIED while a workaround remains in the code path.
- Do not mark REJECTED-AS-PRESSURE to avoid investigating; it requires a
  reproducer and written reasoning like any other record.
- Re-check every CONFIRMED/WORKAROUND record against current
  `mncs-language` before each campaign milestone; language tranches land
  weekly and old restrictions rot fast.
