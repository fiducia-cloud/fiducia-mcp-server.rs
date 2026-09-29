# Conformance

`contracts/` owns structural/wire shape. `conformance/` owns implementation-neutral behavioral expectations and promotion evidence.

1. Shared cases/fixtures are consumed byte-for-byte by every maintained implementation that claims the behavior.
2. Do not keep implementation-local golden corpora for shared behavior.
3. Promotion evidence is bound to exact contract inputs and case revision/digests; stale evidence fails closed.
4. Missing evidence from a required participant is a failure, not a skip.
5. Reports, receipts, snapshots, and parity artifacts are evidence only.
6. Scaffold-only coverage must be explicit; do not claim equivalence until domain cases run for every required participant.
7. Unexplained contract/conformance/runtime drift blocks release and enters evaluation instead of being auto-reconciled.

Next executable layer: `conformance/manifest.v1.json` naming required participants, admission workflow, coverage status, case roots, and contract digests.
