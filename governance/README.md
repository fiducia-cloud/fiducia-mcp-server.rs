# Governance

Contract/conformance boundaries are governed explicitly so downstream consumers do not accumulate competing sources of truth.

1. Every shared interface has exactly one canonical repository owner.
2. Material changes record the decision, alternatives, compatibility impact, migration/rollback path, and conformance evidence.
3. Adding/removing a runtime, language, client, adapter, or persistence projection requires the matching participant/admission update.
4. Generated artifacts cannot silently become a second source of truth.
5. CI/promotion fails closed on missing or stale required evidence; exceptions need a written owner, scope, rationale, and expiry/revisit condition.
6. Emergency changes reconcile authority and evidence before the next release.
7. Cross-repo changes identify canonical ownership instead of copying authority.

When multiple independently maintained participants exist, add a machine-readable `governance/*.v1.json` registry and make CI prove it matches the conformance manifest and admission workflow.
