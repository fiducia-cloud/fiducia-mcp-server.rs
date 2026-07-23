# NATS / JetStream: architecture, invariants, hardening

Audited 2026-07 across the whole org. Only four repos speak NATS; everything
else (including raft) is HTTP. This doc records the intended design, the
invariants the code assumes, and the hardening posture.

## Who connects

| Repo | Role |
|---|---|
| fiducia-messaging.rs | canonical library + `fiducia-relay` binary + compat binary |
| fiducia-messaging (legacy) | older compat service (transactional outbox) |
| fiducia-lambda-service.rs | JetStream lifecycle publishes + Core-NATS container-pool request/reply |
| fiducia-ai-agent-manager.rs | durable file outbox → JetStream, Core-NATS for disposable live progress |

`fiducia-infra` deploys no NATS server; broker config lives out-of-band
(see "Broker-side invariants" — these must be enforced wherever the server
is provisioned).

## Design (the parts to preserve)

- **Division of labor**: NATS JetStream = delivery; fiducia-node = authority
  (leases/fencing); Postgres = state. Effectively-once = fencing token +
  tenant-scoped idempotency key over an at-least-once transport. Raft
  transport is HTTP, never NATS.
- **Subject taxonomy**: `fiducia.<group>.<event>.v<version>`
  (fiducia-messaging.rs `src/subjects.rs`). Identifiers live in the envelope,
  never the subject; UUID tokens in subjects are rejected. One known
  off-taxonomy namespace: `dd.remote.container_pool` (lambda-service
  request/reply).
- **Dedup**: `Nats-Msg-Id` = `v1-` + SHA256(tenant, length-prefixed
  idempotency key).
- **Durable publishes await the JetStream server ack** — a client `flush()`
  is not a persistence guarantee. The ai-agent-manager outbox
  (file-durable, per-record attempts, dead-letter after max attempts,
  never-falls-back-to-Core-NATS for durable events) is the reference
  implementation.
- **`NATS_URL` is environment-only**; CI rejects a `--nats-url` flag, and
  connect-error logging never echoes the URL.

## Broker-side invariants (MUST hold wherever the server is provisioned)

1. **`duplicate_window >= claim_ttl + MAX_PUBLISH_BACKOFF` = 600s.**
   JetStream's default window is 2 minutes; below 600s a relay crash-window
   re-publish is stored as a new message and delivered twice
   (fiducia-messaging.rs `src/outbox.rs` `min_duplicate_window`). The relay
   verifies this at startup and fails closed if the live stream's window is
   too small.
2. **File storage** (not memory) and an explicit replica count for the
   `fiducia.*` stream; retention is limits-based with an explicit max age.
3. **TLS + authenticated connections** (creds/nkey), account isolation per
   environment.

## Client-side hardening policy

- TLS is required for any non-loopback host; loopback may be plaintext for
  dev. `FIDUCIA_NATS_REQUIRE_TLS=1` forces TLS everywhere;
  `FIDUCIA_NATS_ALLOW_PLAINTEXT=1` is an explicit, loudly-logged opt-out.
- `NATS_CREDS_FILE` supplies nkey/JWT credentials without riding the URL.
- The relay has no default NATS_URL — unset config is a startup error, not a
  silent anonymous connect to localhost.
- lambda-service: `FIDUCIA_NATS_STRICT_PUBLISH=1` disables the Core-NATS
  fallback for lifecycle events (fallback use vs. drop are separately
  counted metrics either way).

## Known residual gaps (accepted / tracked)

- lambda-service has no durable outbox; without strict mode its lifecycle
  events remain best-effort by design (the request path must never block on
  messaging).
- JetStream *consumers* are external to these repos (Node.js side); consumer
  ack policy / max-deliver / backoff must be reviewed where they are defined.
- Core-NATS live/progress publishes are intentionally at-most-once.
