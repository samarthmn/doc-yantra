# Larkspur Platform Engineering Guide

This guide covers day-to-day engineering practice on **Larkspur**, Kavera
Systems' order-orchestration platform. It is the *canonical* onboarding
reference: if a wiki page and this document disagree, this document wins.
Send corrections to the `#platform-guild` channel or open a PR against
`docs/engineering-guide.md`.

## 1. Architecture overview

Larkspur is a modular monolith with four extracted services. Everything
communicates over gRPC internally and exposes REST at the edge via
`gateway-api`. State lives in PostgreSQL 16; ephemeral coordination goes
through Redis.

- **gateway-api** — edge REST/auth layer, rate limiting, request signing
- **order-core** — the monolith: cart, pricing, checkout, order lifecycle
- **fulfil-svc** — warehouse routing and carrier label purchase
- **ledger-svc** — double-entry bookkeeping for every money movement
- **notify-svc** — email/SMS/webhook fan-out with per-tenant throttles

### 1.1 Service inventory

| Service     | Language | Owns datastore     | On-call rotation |
| ----------- | -------- | ------------------ | ---------------- |
| gateway-api | Go       | none (stateless)   | Platform         |
| order-core  | Kotlin   | `orders_pg`        | Commerce         |
| fulfil-svc  | Kotlin   | `fulfilment_pg`    | Commerce         |
| ledger-svc  | Rust     | `ledger_pg`        | Payments         |
| notify-svc  | Go       | Redis streams only | Platform         |

> Rule of thumb: a service owns its tables outright. Cross-service reads go
> through the owning service's API, never through a shared connection string.
> The single historical exception (`reporting_ro`) is read-only and frozen.

## 2. Local development

### 2.1 Prerequisites

1. Install mise (`curl -fsSL https://mise.jdx.dev/install.sh | sh`) and run
   `mise install` at the repo root; it pins Go, Kotlin/JDK, and Rust.
2. Install Docker Desktop or Podman 5+ with the compose plugin.
3. Request a `dev-seed` token from Vault (see
   [Vault access runbook](https://wiki.kavera.dev/runbooks/vault-access)).
4. Clone the monorepo and bootstrap:

```bash
git clone git@github.corp.kavera.dev:kavera/larkspur.git
cd larkspur
./scripts/bootstrap.sh          # compiles protos, seeds local Postgres
docker compose up -d postgres redis mailpit
./scripts/dev.sh order-core     # hot-reload loop for one service
```

### 2.2 Environment variables

The bootstrap script writes `.env.local`; the values below are the ones
people actually need to change.

| Variable            | Default                  | Notes                        |
| ------------------- | ------------------------ | ---------------------------- |
| `LARKSPUR_ENV`      | `local`                  | never set to `prod` locally  |
| `ORDERS_PG_URL`     | `postgres://localhost/…` | seeded with fixture tenants  |
| `FEATURE_FLAGS_URL` | in-memory stub           | point at staging to mirror   |
| `STRIPE_MODE`       | `mock`                   | `test` hits Stripe test mode |

Secrets are **never** committed. `direnv` loads `.env.local`, and the
pre-commit hook rejects anything matching `sk_live_` or `-----BEGIN`.

## 3. Branching and code review

- Branch from `main`; name branches `feat/…`, `fix/…`, or `chore/…`.
- Keep PRs under ~400 changed lines; split mechanical renames from logic.
- Two approvals for changes touching `ledger-svc`; one elsewhere.
- Squash-merge only. The squash title becomes the changelog line, so write
  it in imperative mood: *"Add idempotency keys to refund endpoint"*.
- CI must be green — no admin-merges. Flaky tests get quarantined via
  `@Quarantined("LARK-1234")`, never deleted.

## 4. Deployment pipeline

Merges to `main` build once and promote the same artifact through three
environments. Promotion is automatic to staging and manual to production.

1. `build` — container images tagged with the git SHA, SBOM attached
2. `staging` — auto-deploy, smoke suite (`./scripts/smoke.sh staging`)
3. `canary` — 5% of production traffic for 30 minutes, auto-rollback on
   SLO burn
4. `production` — full rollout, region by region (`eu-1`, then `us-1`)

```yaml
# .larkspur/deploy.yaml — canary gate (excerpt)
canary:
  traffic_percent: 5
  hold_minutes: 30
  rollback_on:
    - slo: checkout-latency-p99
      burn_rate: 2.0
    - slo: ledger-write-errors
      burn_rate: 1.0
```

> **Change freeze:** no production promotions from 22:00 Friday to 08:00
> Monday (Europe/Berlin), and none during the Black Friday window published
> by the release captain. Staging is always open.

## 5. Observability

Every service emits OpenTelemetry traces; the collector fans out to Tempo
and Prometheus. Dashboards live in
[Grafana](https://grafana.kavera.dev/d/larkspur-home) under the *Larkspur*
folder — clone a dashboard before editing, the provisioned ones are
overwritten on deploy.

- Log through the shared `koru` logger; plain `println` never ships.
- Attach `tenant_id` and `order_id` as span attributes, not log text.
- Alert rules live in code (`observability/alerts/*.rules.yaml`) and are
  reviewed like any other change.

### 5.1 Alert severity ladder

1. **P1** — customer-visible outage or money movement halted; page now.
2. **P2** — degraded but functional (elevated latency, one region down).
3. **P3** — no customer impact; fix within the sprint. Ticket only.

## 6. Incident response

When paged, acknowledge within five minutes, then follow the ladder:

1. Declare in `#incidents` using `/incident declare` — severity honest,
   not optimistic.
2. Stabilise first: roll back before you debug. `./scripts/rollback.sh
   --service order-core --to <sha>` completes in about 90 seconds.
3. Keep a timeline in the incident doc as you go; screenshots of graphs
   beat memory.
4. Post-incident review within five working days — blameless, with
   follow-up actions owned and dated.

## 7. Coding standards

- Prefer boring code: a `for` loop you can read beats a clever stream
  pipeline you cannot.
- Public APIs take and return DTOs, never entity classes.
- Feature flags are temporary; every flag gets a removal ticket at birth.
- Database migrations are expand/contract — *never* drop a column in the
  same release that stops writing it.

```kotlin
// Idempotency: every mutating endpoint takes a client key.
@PostMapping("/v1/refunds")
fun createRefund(
    @RequestHeader("Idempotency-Key") key: IdempotencyKey,
    @RequestBody body: RefundRequest,
): RefundResponse = refunds.createOrReplay(key, body.toCommand())
```

---

*Maintained by the Platform Guild. Last reviewed 2026-07-10; review cadence
is quarterly. Historical revisions live in git — this file has no
"changelog" section by design.*
