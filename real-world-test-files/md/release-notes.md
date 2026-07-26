# Larkspur Release Notes

All notable changes to the Larkspur platform. Versions follow semver;
dates are deployment-to-production dates (region `eu-1`).

## v2.9.0 — 2026-07-21

### Added

- Partial refunds: `POST /v1/refunds` accepts `amount_cents` below the
  original charge; ledger entries split automatically.
- Carrier auto-failover in fulfil-svc — if label purchase fails twice, the
  next-cheapest carrier in the tenant's rate card is tried.
- Webhook redelivery UI for tenant admins (up to 72 hours back).

### Changed

- Checkout price recalculation now runs on the write path; the async
  reconciler is retained as a safety net only.
- Default API rate limit raised from 60 to 120 requests/minute per token.

### Fixed

- Duplicate `order.completed` webhooks when a fulfilment retry raced the
  completion event (LARK-2141).
- Timezone drift in the daily settlement report for tenants west of UTC-6
  (LARK-2156).

## v2.8.1 — 2026-07-02

### Fixed

- ledger-svc: rounding of multi-currency fees now uses banker's rounding
  consistently; historical entries are unaffected (LARK-2130).
- gateway-api: `Retry-After` header was missing on 429 responses.

### Security

- Bumped `jose` to close a token-confusion advisory; no exploitation
  observed in audit logs.

## v2.8.0 — 2026-06-16

### Added

- Multi-warehouse split shipments — one order can fulfil from up to three
  sites, with per-shipment tracking events.
- `GET /v1/orders/{id}/timeline` returning a merged audit trail.

### Changed

- notify-svc throttles are now per-tenant *and* per-channel; the old
  global cap is removed.

### Deprecated

- `POST /v1/orders/{id}/cancel` without a `reason` body is deprecated and
  will require it from v3.0. A `Deprecation` header is emitted now.

### Fixed

- Slow-query regression on the order search endpoint introduced in v2.7.0
  (missing composite index, LARK-2087).

## v2.7.2 — 2026-06-03

### Fixed

- Hotfix: canary rollback failed to restore feature-flag state when the
  flag service was itself being deployed (LARK-2071).

## Compatibility matrix

| Larkspur | API schema | Postgres | Node SDK  | Kotlin SDK |
| -------- | ---------- | -------- | --------- | ---------- |
| v2.9.x   | 2026-07-01 | 15, 16   | >= 4.2    | >= 3.8     |
| v2.8.x   | 2026-05-15 | 15, 16   | >= 4.0    | >= 3.6     |
| v2.7.x   | 2026-04-02 | 14, 15   | >= 3.9    | >= 3.4     |

Upgrading skips at most one minor version: to move from v2.7 to v2.9,
deploy v2.8 first and let its migrations complete. See the
[upgrade runbook](https://wiki.kavera.dev/runbooks/larkspur-upgrade) for
the expand/contract sequence.
