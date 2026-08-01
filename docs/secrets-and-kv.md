# Secrets & KV: where customer data actually lives

Short answer: customer secrets/config are **durably persisted and encrypted at
rest** in fiducia's own Raft-replicated KV. Nothing secret is "just in memory",
and nothing secret is in Supabase.

## The store

The KV store is implemented in `fiducia-node.rs` and served at `/v1/kv`
(GET/PUT/DELETE, plus `?watch=true` SSE). Writes go through Raft
(`Command::KvPut`); the state machine is in memory, but every write is
persisted to an fsync'd Raft log and periodic snapshots **before it is
acknowledged**, so a pod restart recovers everything.

- On-disk layout: `<FIDUCIA_DATA_DIR>/shard-<id>/{meta,log,snapshot}`
  (write-tmp → fsync → rename → fsync(dir); see `fiducia-node.rs/src/persist.rs`).
- `FIDUCIA_DATA_DIR` defaults to `/var/lib/fiducia`.
- In Kubernetes this is a per-node **10Gi PersistentVolumeClaim** mounted at
  `/var/lib/fiducia` (`fiducia-infra/base/node/statefulset.yaml`
  `volumeClaimTemplates`), replicated across nodes by Raft.

There is **no Postgres, Redis, or NATS JetStream KV** behind the secret store.
Postgres/Supabase appear elsewhere for other domains only:

| System | Used for |
|---|---|
| fiducia-node KV (Raft, PVC) | customer config KV **and secrets**, auth API-key records |
| Supabase (Postgres) | dashboard identity / JWT sessions only |
| fiducia-messaging Postgres | transactional outbox/inbox, dedup, retention |
| fiducia-memory Postgres | agent memory (RLS-enforced) |
| fiducia-sync Postgres | change journal for local-first sync |

## Encryption at rest

Values are **sealed before entering the Raft log** — the log, snapshots, and
the in-memory state machine all hold ciphertext. Sealing happens once on the
receiving node, so replicas store byte-identical ciphertext.

Exactly one backend is selected via env (`KvCipher::from_env` in
`fiducia-node.rs/src/kv.rs`), and configuration is **fail-closed**: a partial
config refuses to start rather than silently storing plaintext, and unreadable
ciphertext returns `kv_protection_unavailable` rather than falling back.

- **HashiCorp Vault Transit** — `FIDUCIA_KV_VAULT_ADDR/_TOKEN/_KEY/_MOUNT/_NAMESPACE`.
  Vault owns the key material; fiducia stores only `fcenc:vault:v1:` envelopes.
  The encryption context binds ciphertext to the org-scoped storage key.
- **Local AES-256-GCM keyring** — `FIDUCIA_KV_ENCRYPTION_KEYS` (JSON
  key-id → base64 32-byte key) + `FIDUCIA_KV_ENCRYPTION_ACTIVE_KEY_ID`;
  legacy single-key `FIDUCIA_KV_ENCRYPTION_KEY` supported for migration.
  Envelopes `fcenc:v2:` / `fcenc:v1:`.

A write may opt out per-key with `{"plaintext": true}`; the default is
encrypted whenever a backend is configured. Operator/rotation runbook:
`fiducia-infra/docs/kv-protection.md` (the `fiducia-kv-protection` Kubernetes
Secret is populated by External Secrets / CSI / Vault Agent — manifests never
contain key material).

## Tenant scoping

Every key is namespaced into the caller's org (`OrgScope`); the load balancer
injects `x-fiducia-org-id` from authenticated identity and strips any
client-supplied internal headers. The auth subsystem itself dogfoods the KV:
`fiducia-auth.rs` persists API-key records (secret **hashed**, never
reversible) under the reserved `__auth/keys/{key_id}` and
`__auth/orgs/{org_id}/keys` keyspace — the end-user data plane never touches
Supabase.

## How consumers ingest secrets (Vault-style)

Consumers of the PaaS/SaaS API pull secrets over the same authenticated KV
API — analogous to reading from Vault's KV engine:

1. **Read at boot**: `GET /v1/kv?key=<name>` (or `?prefix=` for a batch) with
   `Authorization: Bearer <api key or JWT>`. The response returns plaintext
   after server-side decryption plus `protection.at_rest` metadata so the
   consumer can verify at-rest protection was active.
2. **Live rotation**: subscribe `GET /v1/kv?key=<name>&watch=true` (SSE) and
   hot-reload on change — no restart needed for secret rotation.
3. **Recommended client behavior** (see `fiducia-infra/docs/kv-protection.md`):
   allowlist the exact keys you read, let process env vars take precedence,
   never log values.

The official clients in `fiducia-clients` expose config-KV operations in all
supported languages, so ingestion does not require hand-rolled HTTP.

## MCP server relevance

The `kv_get` tool in this MCP server reads the same `/v1/kv` surface
(key xor prefix). It is read-only and honors the same org scoping via the
configured auth headers.
