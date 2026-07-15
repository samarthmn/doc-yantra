# Agent tasks 6B — on-demand fonts via Fontsource (`gdocs-anchors-toc` gate)

> **STATUS: planning complete (owner-approved, 2026-07-15 — third revision: Fontsource provider, minimal local fonts). Implementation not started.** Depends on **6A complete** (`agent-tasks-6a-fontless-package.md`). Together with 6A this replaces all earlier phase-6 designs (metric overlay → owner catalog/broker → **this**).

**Owner's operating principle (verbatim intent):** *discover which font the document needs → fetch it → inject it → produce the perfect PDF.* Almost nothing ships locally: the office package stays fontless (6A gate), and the worker carries exactly **one local fallback family** used only when font loading fails. Everything else is fetched on demand and cached.

## ⚠️ Release coupling (restated from 6A)

6A and 6B ship in **one** `0.0.x` release; nothing is tagged between them. The release also waits on the DocKosha worker follow-ups (§G) so a fontless package never reaches production without its resolver configured.

## §A The `FontsourceProvider` (host layer — implements 6A's `FontProvider` interface)

Fontsource packages the Google Fonts library (plus extras) as **semver-versioned, immutable** artifacts, with a direct **TTF** route — SFNT bytes the core parses natively (the jsDelivr font CDN itself serves woff2 only; do not use it for engine bytes):

```
GET https://api.fontsource.org/v1/fonts/{id}                                  # metadata: subsets, weights, styles, version
GET https://api.fontsource.org/v1/fonts/{id}/{subset}-{weight}-{style}.ttf?version={pkgVersion}
```

1. **Request mapping.** `FontRequest → fontsource id` via a provider-config alias table (host concern; core untouched): normalized family name → id, plus the compatibility aliases so every substitution target is *fetchable*: `Calibri→carlito`, `Cambria→caladea`, `Arial|Helvetica|Segoe UI|Liberation Sans→arimo`, `Times New Roman|Liberation Serif→tinos`, `Courier New|Liberation Mono→cousine` (Arimo/Tinos/Cousine are the metric-compatible Croscore lineage Liberation 2.x derives from — Liberation itself is not on Google Fonts/Fontsource). Weight/italic map to Fontsource's weight/style axes; static packages only in v1 (**variable packages excluded** — the engine's face model has no axis instancing; revisit later).
2. **Subsets.** Fontsource files are per-unicode-subset. Preflight's `FontRequest.scripts` selects which subset TTFs to fetch (always the family's `defSubset`, plus subsets covering the requested scripts); each fetched subset registers as a sibling face of the same family — the existing `coverage_fallbacks` machinery selects per-character among them. Coverage validation runs against the union.
3. **Version policy — "forget versioning", with two pins.** Default: `?version=latest` per family (owner accepts that Fontsource family updates may change output over time — recorded in §F). Exactly two config pins exist: `ubuntu` and `ubuntu-mono`, pinned to the package versions whose internals verify as the **classic 0.83 / 0.80 lineage** (H6 one-time check). Rationale: the 2022–23 Ubuntu redesign shipped as the *separate* "Ubuntu Sans" family, so Fontsource's `ubuntu` is expected to be the classic lineage the ground truth embeds — the pin guards against future re-cuts, and the fidelity gate is the empirical proof.
4. **Fallback source.** Config-listed secondary: `https://cdn.jsdelivr.net/gh/google/fonts@{pinned-commit}/{license-dir}/{family}/…​.ttf` (full-cmap TTFs, jsDelivr-immutable) used when the API is down or lacks a family. Same validation path.
5. **Status mapping.** HTTP 404 → `not_found`; 5xx/timeout/network → `temporary_failure`; malformed/oversized payloads → rejected by 6A validation (`font_identity_mismatch`/structure errors). `denied` is unused for Fontsource (everything it serves is OFL/UFL/Apache) but remains in the contract for future providers.
6. **Security/limits** (6A §F applies): HTTPS-only, timeout, max bytes, redirect ≤3, bounded parallelism, `AbortSignal`, no document text, no URL built from unsanitized document strings — family names pass through the alias table/normalizer, never raw into a URL path.

## §B The single local fallback family (owner delegated the choice)

**Noto Sans — Regular, Bold, Italic, Bold Italic (static TTFs, SIL OFL 1.1).** Chosen for: the widest Latin/Greek/Cyrillic + common-symbol coverage per byte (bullets U+2022/U+25CF/U+25AA matter for Google-Docs lists — pin a coverage test at implementation), neutral metrics, unambiguous licensing, and its universal role as fallback-of-last-resort.

- Ships as **worker/deployment assets** (wasm-worker asset dir or the DocKosha deploy) — never inside the office wasm/npm (the 6A fontless gate enforces this), boot-registered at worker start.
- Used **only** on resolution failure: provider unreachable + cold cache, `not_found` family with no alias, or validation rejection. Compatible mode then converts with Noto Sans and emits `font_fallback_used` (visible warning, never labeled exact). **Strict mode never touches it** — it fails with `FontUnresolved` as specced.
- It replaces the earlier "boot-register the OFL trio" plan: Carlito/Caladea/Arimo/etc. are now *fetched* like everything else (first use per worker warms the persistent cache, after which they behave as local). 6A §C2 is amended accordingly.

## §C The pipeline (unchanged mechanics, restated with this provider)

`office_font_requests` → alias/normalize → memory cache → persistent cache → FontsourceProvider (batched, deduped, in-flight-shared) → 6A validation (`register_font_validated`: structure, name-table identity, version recorded, fsType, coverage) → registered exact faces → **one** layout pass → subset → embed (`/W`, ToUnicode, selection geometry all from the fetched font — no `TJ` regression). No layout before resolution completes; no reflow-after-swap. Profile/convert parity per 6A §B sequence.

## §D Caching (6A §C3 applied)

Content-hash store + request-index keyed by `normalized(family,weight,italic,subset)+versionPolicy+provider`. Cold: one API call per unique face-subset. Warm: zero network — in steady state the worker behaves as if fonts were local. Offline+warm succeeds; offline+cold → deterministic fallback (compatible) / `FontUnresolved` (strict). Brief negative-TTL. Nothing font-shaped in the package.

## §E Fidelity gate

- `run --case gdocs-anchors-toc`: pages 10/10, **min page SSIM ≥ 0.90** (baseline to beat: 0.8973/0.8114/0.6899/0.9193/0.9222/0.9066/0.7909/0.9889/0.8496/0.9102); the emitted PDF must embed subsets derived from the fetched Ubuntu bytes; verify line breaks + anchor positions, not just SSIM.
- Full corpus afterward: zero changes to ground truths, thresholds, goldens, manifests, rasterization; no regression anywhere (`complex` 0.9374 floor, `gdocs-columns` 0.9280). Native harness still uses dev bundled fonts (6A) — unaffected by provider policy.
- If the gate lands short *with verified-0.83 fonts*, the residue is geometry → phase 7's probe; no re-baselining.

## §F Accepted risks (owner-accepted, recorded — not hidden)

1. **Version float:** `latest` families may update upstream and change layout for identical documents over time. Accepted by owner ("forget versioning"); the two Ubuntu pins are the only exceptions. Telemetry records resolved versions per conversion so any drift is attributable.
2. **Third-party availability:** `api.fontsource.org` is community-run; mitigations = hard caching (§D), the jsDelivr/gh fallback source (§A4), and the Noto net (§B). Cold-cache conversion latency includes fetch RTT (telemetry: fetch duration).
3. **Privacy:** per-document font names flow to Fontsource/jsDelivr at fetch time (not to Google, not document text). Accepted posture for DocKosha.
4. **Subset partiality:** per-subset files mean rare codepoints outside fetched subsets fall to coverage fallbacks/Noto; preflight-driven subset selection bounds this.
5. **CI provisioning decision remains open** (unchanged options): mock provider + local fixture fonts, or seeded cache; CI never calls live CDNs either way.

## §G DocKosha follow-ups (cross-repo, for the pin bump — no doc-kosha changes here)

1. Worker: deploy Noto Sans fallback assets + boot registration; configure FontsourceProvider (endpoint, pins, alias table, cache dir, limits).
2. Router adopts the 6A §B sequence (requests → register → profile → convert).
3. Pin bump to the combined 6A+6B release.

## Human task H6 (revised again — see `human-tasks.md`)

One-time: fetch Fontsource's `ubuntu` + `ubuntu-mono` TTFs, read their internal version strings (registration report or `ttx`/`otfinfo`), confirm classic 0.83/0.80 lineage, and record the pinned package versions in the provider config. Obtain the four Noto Sans static TTFs for the worker's fallback assets (deployment, not repo/package). Settle the CI provisioning choice (§F5).

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
node --test tests/                                    # mock provider; fontless gate; fallback-path tests
cargo run -p fidelity -- run --case gdocs-anchors-toc # ≥ 0.90, 10/10 pages
cargo run -p fidelity -- run --format docx            # full corpus, zero regressions
DRY_RUN=1 pdf-core/scripts/release.sh                 # combined 6A+6B release preview only
```
