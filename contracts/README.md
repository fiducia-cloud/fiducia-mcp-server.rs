# Contract authority

`contracts/` is the human-authored authority boundary for external API, wire, persistence, configuration, and cross-process shapes owned here.

1. TypeSpec and JSON Schema/OpenAPI, when both used, are independent peer authorities; neither is generated from the other and promoted as canonical.
2. Generated SDKs, types, ORM projections, docs, and runtime descriptors are evidence/projections, not authority.
3. Material contract changes must state compatibility impact and identify conformance evidence.
4. Unexplained drift between peer authorities, generated witnesses, persistence projections, or runtime behavior fails closed and blocks promotion/release.
5. Cross-repository interfaces have exactly one canonical owner; mirrors and consumers must not silently fork them.
6. Wire names and compatibility semantics survive implementation-language naming and reserved-word escaping.

Until domain-specific contracts are added here, this file establishes the authority boundary only and claims no behavioral coverage.
