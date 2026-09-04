# Fixture provenance ledger

Only synthetic or project-authored fixtures, and fixtures with an explicit
rights clearance, may be exported in a public source candidate. The exporter
fails closed for unresolved material.

| Area | Status | Public-candidate rule |
| --- | --- | --- |
| `tests/fixtures/` | project-authored/synthetic | eligible after audit |
| `mock-files/` | unresolved | omitted until individually approved |
| `fidelity-corpus/` | mixed and unresolved | omitted until each entry is approved |
| `real-world-test-files/` | unresolved | omitted until individually approved |
| owner-supplied Microsoft Word goldens | pending owner workflow | do not export until the owner records source, license, and hash |

The Word golden workflow is owner-supplied: the owner creates the document in a
licensed Microsoft Word environment, records the exact Word/platform/version,
confirms rights to redistribute both source and exported PDF, records SHA-256
hashes and the manifest entry, then requests review. Fidelity thresholds are
not lowered while provenance is pending.
