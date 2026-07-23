# Admin & customer web stacks (MASH) and browser testing

MASH = Maud, Axum, Supabase, SeaORM (over sqlx), HTMX. Both web apps follow
it; this doc records how, where they diverge, and how they are tested.

## The two apps

| | fiducia-admin.rs | fiducia-customer.rs (crate `fiducia-backend`) |
|---|---|---|
| Port | 8096 | 8080 |
| Framework | Axum 0.7 (`ws`) + tower-http | Axum 0.7 (`ws`) + tower-http |
| Templates | Maud (`src/views.rs`) | Maud (server-rendered `/app/*`) |
| DB | SeaORM 1.1 (sqlx-postgres runtime underneath; no raw sqlx) — isolated admin-plane Postgres | SeaORM 1.1 + generated `fiducia-interfaces-db` row types |
| HTMX | vendored `/assets/htmx.min.js` compiled into the binary; polling panels (`hx-trigger="every 5s"`) | vendored htmx; progressively-enhanced forms (work no-JS) |
| Realtime | WS `/admin/ws` streaming fiducia-sync change frames via `fiducia-sync-core` + `fiducia-sync.js` → IndexedDB | custom WS `/app/ws` (heartbeat JSON) + SSE `/app/events` |
| Supabase | password grant for login only | password grant + magic-link/email OTP, phone OTP, TOTP MFA relay (`src/supabase_auth.rs`) |
| Session | `__Host-` HttpOnly cookie; role gate (admin/operator) via fiducia-auth | HttpOnly SameSite=Strict cookie; org-scoped via fiducia-auth `GET /v1/me` |

Shared posture: Supabase is auth-only (JWKS-verified sessions via
fiducia-auth; no PostgREST, no storage — org/plan data is cached in-cluster
by fiducia-auth). Secrets/config come from env; htmx is self-hosted, no CDN.

`fiducia-customer-ui.web` (Vite SPA) is ARCHIVED upstream; the canonical
customer frontend is the server-rendered Maud+htmx surface in
fiducia-customer.rs. The SPA survives only as an optional static `dist/`
mount.

## Known divergences from "optimal MASH" (tracked)

1. Neither app uses htmx's official `ws` extension (`hx-ext="ws"`); realtime
   swaps are hand-rolled JSON streams instead of htmx-swappable fragments.
   fiducia-sync's `hx-ext="fiducia-optimistic"` extension covers the
   optimistic-write path; a fragment-over-WS path remains an open item.
2. The two apps have divergent realtime transports (admin: shared
   fiducia-sync protocol; customer: bespoke heartbeat WS + parallel SSE).
   Converging customer onto fiducia-sync-core is the intended direction.
3. Local orchestration is Node-based (`fiducia-e2e/scripts/dev-stack.mjs`
   boots real auth/admin/customer + stub Supabase/brain/KV + throwaway
   Postgres) — no docker-compose for the web tier.

## Browser testing

Playwright and Puppeteer are both used **as libraries under `node --test`**
(no playwright.config, no Playwright runner):

- Per-repo: `fiducia-admin.rs/tests/admin-{playwright,puppeteer}.test.mjs`
  (+ harness) and the mirrored `fiducia-customer.rs/tests/customer-*` suite —
  each boots its own server via cargo with stubs from `@fiducia/test-config`.
- Cross-app: `fiducia-e2e/tests/browser/` (login journeys, customer MFA) and
  `tests/webapps/` (HTTP-contract auth separation), gated by
  `FIDUCIA_E2E_BROWSER=1` / `FIDUCIA_E2E_WEBAPPS=1`.
- Shared stubs (Supabase/KV/brain) live in `fiducia-test-config`
  (`@fiducia/test-config`, `src/stubs.mjs`).

Coverage expectations for each app's suite: SSR render + htmx asset,
progressive enhancement, a full auth journey against stub Supabase, session
cookie attributes, a CSRF negative path, and realtime (WS/SSE) connectivity.
