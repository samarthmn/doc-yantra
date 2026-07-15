# Agent tasks 6 — metric-compatibility overlays (unblocks `gdocs-anchors-toc`)

**Decision this implements (owner, 2026-07-15, planning thread):** instead of vendoring Ubuntu TTF binaries, the engine gains **metric-compatibility overlays** — per-family advance/vertical-metric data shipped as compiled-in tables, applied over a substitute face's outlines. Wrap points and line heights become those of the requested font; inking comes from the closest bundled face. TTF vendoring (UFL-1.0 permits it) is **pre-approved as the contingency** if any `gdocs-anchors-toc` page stays below 0.90 after the overlay lands (see T6.7).

Why this works here: Word breaks lines from advance widths; pair kerning applies only when `w:kern` is set (already gated in `shape.rs` — and Google Docs exports never set it), and the engine shapes with `liga(false)` always. So for these documents, **wrap parity is a pure function of advances** — which is exactly what an overlay supplies. Expected outcome is the Carlito↔Calibri precedent: 0.93+ pages with different outlines. All code references are against branch commit `e1ca853`; the stopped worktree patch (see step 0) may shift lines — re-locate by symbol.

## Step 0 — commit the stopped remediation first (precondition, both phases)

The 21-file uncommitted worktree described in `implementation-handoff-2026-07-15/01-current-status.md` must be reviewed, verified (`cargo fmt --check`, `clippy`, `cargo test --locked`, wasm build, `node --test tests/` — investigating the two known drifts rather than masking them), and **committed** before phase-6/7 work begins. Do not start these tasks on top of uncommitted state.

## T6.1 — The metrics database

**Format (compiled-in, generated Rust — no runtime IO, wasm-transparent):**

```rust
// office-core/src/fonts/metrics_db.rs  (registry, hand-written)
pub struct OverlayEntry {
    /// Family aliases matched case-insensitively ("Ubuntu").
    pub families: &'static [&'static str],
    pub weight: u16,          // 400 | 700
    pub italic: bool,
    /// Outline carrier: substitute family whose glyphs render the text.
    pub outline_family: &'static str,
    pub units_per_em: u16,
    /// OS/2 usWinAscent / |usWinDescent| and hhea lineGap, in font units
    /// (the exact values FaceMetrics::new prefers — line-height parity).
    pub win_ascent: i16, pub win_descent: i16, pub line_gap: i16,
    /// OS/2 sub/superscript y-offsets + underline position/thickness (units).
    pub subscript_y: i16, pub superscript_y: i16,
    pub underline_pos: i16, pub underline_thickness: i16,
    /// Sorted (codepoint, advance-in-units) pairs; binary-searched.
    pub advances: &'static [(u32, u16)],
}
pub fn lookup(family: &str, weight: u16, italic: bool) -> Option<&'static OverlayEntry>;
```

- **Codepoint set per entry:** Basic Latin 0x20–0x7E, Latin-1 0xA0–0xFF, General Punctuation 0x2010–0x203A + 0x2044 + 0x20AC, bullets/marks 0x2022/0x2026/0x2122. ≈300 pairs ≈ 1.8 KB/style — 8 Ubuntu styles ≈ 15 KB total. Negligible wasm delta.
- **Generator:** `office-core/src/bin/extract_metrics.rs` (native cargo bin, dev-only): input TTF path(s) → prints the generated module (e.g. `src/fonts/metrics_db/ubuntu.rs`) using `ttf_parser` (already a dependency — read cmap + hmtx + OS/2 + hhea + post; zero new crates). Header comment records: source filename, SHA-256, font version string, license (UFL-1.0), generation date. **Metrics data is committed; TTFs are not.**
- **First entries (via human task H6):** Ubuntu Regular/Bold/Italic/BoldItalic v0.83 → `outline_family: "Carlito"` (humanist sans — closer inking to Ubuntu than grotesque Liberation Sans; revisit per H3 heatmaps); Ubuntu Mono R/B/I/BI v0.80 → `outline_family: "Liberation Mono"`. These versions match the fixture's ground-truth embeds — do not substitute the current redesigned Ubuntu release.

## T6.2 — Resolver ladder step 3.5 (`fonts/matching.rs`)

`resolve()` currently: exact → `substitute()` map → `fallback_class()`. Insert between steps 3 and 4:

```rust
// 3.5: metric-compatibility overlay (advances from the requested family,
// outlines from the entry's carrier face).
if let Some(entry) = metrics_db::lookup(family, weight, italic)
    && let Some(pick) = pick_in_family(db, entry.outline_family, weight, italic)
{
    out.face = Some(pick.face);
    out.overlay = Some(entry);            // new field: Option<&'static OverlayEntry>
    out.warnings.push(Warning::new(
        "font_metrics_overlay",
        format!("{family} metrics over {} outlines", entry.outline_family),
    ));
    return out;
}
```

- `Resolved` gains `pub overlay: Option<&'static OverlayEntry>` (Default `None`).
- The overlaid family must **not** be recorded in `missing` and must not emit `font_fallback`.
- `fontsMissing` stats and the DocProfile therefore stop listing Ubuntu; the new warning code is the telemetry signal. Do NOT invent warnings that never fire (repo lesson: the removed `superscript_synthesized`) — add the code to docs only alongside the emitting code path.

## T6.3 — Advance substitution at the shaping boundary (`text/shape.rs`)

Add an overlay-aware wrapper (keep `shape`/`shape_with_fallback` signatures for existing callers):

1. Shape exactly as today (HarfRust, `liga(false)`, `kern` as passed).
2. Post-pass when `overlay` is `Some`: for each `Glyph`, take its cluster's source scalar (the existing `unicode: Option<String>` — first scalar of the cluster; multi-scalar clusters fall back to the carrier advance), look up the DB advance, and if present replace `advance_pt = db_units as f64 / entry.units_per_em as f64 * size_pt + spacing` (character spacing applied per cluster exactly as the current code does). Recompute `width_pt`.
3. **Pair kerning is disabled under an overlay** even when `w:kern` requests it (the carrier's pairs are not the requested font's pairs; zero is closer than wrong). Emit detail on the overlay warning when kerning was requested-and-dropped. Google fixtures are unaffected (no `w:kern`).
4. Coverage semantics unchanged: `missing_glyphs` still comes from the carrier's cmap; a codepoint absent from the DB but present in the carrier renders with the carrier advance (documented-lossy; no extra warning per glyph).

**Thread the overlay** through every shaping call site: `layout/paragraph.rs` run shaping (the resolved font struct it carries), `layout/numbering_layout.rs::build_label`, header/footer paths (same paragraph entry), and the profile coverage check `profile.rs::add_fallback_face_missing_reason` (pass-through so profile and layout shape identically — parity).

## T6.4 — Vertical metrics under overlay (`fonts/metrics.rs` + `layout/paragraph.rs`)

Line height must be the requested font's, not the carrier's. Where `FaceMetrics::new` supplies `ascent/descent/line_gap` (OS/2 win metrics + hhea gap — the documented Word parity lever), introduce an overlay-aware constructor: `FaceMetrics::with_overlay(face, entry)` overriding `units_per_em`, `ascent`, `descent`, `line_gap`, and the `underline_pt`/`subscript`/`superscript` accessors from `OverlayEntry`. Callers that compute line heights, underline decor, and script offsets for a resolved run must select it when the run resolved with an overlay. `LAYOUT_CALIBRATION.md` gets an entry only if any constant is adjusted during H3 calibration.

## T6.5 — Emitter: `/W` arrays must carry overlay advances (`emit/pdf.rs`)

The emitter's invariant is **no TJ adjustments** (there is a test asserting `!stream.contains("TJ")` — PDF.js geometry relies on `/W`), and `EmbeddedFont::cid_widths` drives both `/W` (`write_font`, consecutive widths) and width bookkeeping (`painted_cid` sites, `natural_width_1000`). Therefore:

1. Key the embedded-font registry by **(FaceId, overlay-id)** instead of FaceId alone. An overlaid usage produces its own `EmbeddedFont` whose `cid_widths` are populated from the DB (`advance_1000`-style: `db_units / units_per_em * 1000`, rounded exactly as `FaceMetrics::advance_1000` rounds), falling back to the subset's face width for chars outside the DB — the same fallback rule as T6.3, so layout advances and `/W` agree glyph-for-glyph.
2. The font **program** (FontFile2 subset) may be shared between the plain and overlaid instances via an indirect object if convenient; two font dictionaries with distinct `/W` are otherwise cheap. ToUnicode is built per instance as today (source chars unchanged — extraction/copy stays exact).
3. Extend the no-TJ test to a fixture using an overlay; add a selection-geometry regression on the overlaid fixture through the existing PDF.js/pdfium test lanes (the repo already regresses "line ownership and selection geometry").

## T6.6 — Profile parity and stats

- `profile_docx`'s font resolution (`resolve_fonts`) uses the same `resolve()` — it picks up overlays automatically; assert in a test that profile and convert produce identical warnings for an Ubuntu fixture.
- `envelope_parity_test` must stay green unchanged (overlay never changes a verdict — it only removes `font_fallback`+`missing` cases into supported-with-warning).

## T6.7 — Gate and contingency ladder

1. Rebuild wasm (`wasm/build.sh`), rerun the package suite, then: `cargo run -p fidelity -- run --case gdocs-anchors-toc`. **Gate: min page SSIM ≥ 0.90** (handoff baseline to beat: 0.6899/p3; pages 2/7/9 at 0.81/0.79/0.85).
2. If a page lands 0.88–0.90: try switching that entry's `outline_family` (Carlito ↔ Liberation Sans) per heatmap evidence; one calibration commit + log entry.
3. If still short: execute the **pre-approved contingency** — human-vendors the eight Canonical TTFs (UFL-1.0, license file alongside; update `office-core/fonts/README.md` and the face-count test), registered via `FontDb::with_bundled` — the exact-family path (`pick_in_family`) then wins before the overlay step and the overlay entry becomes dormant for Ubuntu. Record the wasm-size delta.
4. Full corpus re-run: no regression anywhere else (`complex` floor 0.9374, `gdocs-columns` 0.9280).

## Tests (unit + integration)

- DB: lookup by alias/weight/italic; binary-search advance; generator round-trip on a bundled Liberation face (generate → compare against `ttf_parser` reads).
- Shaping: overlaid advance replaces carrier advance; char-spacing still added once per cluster; kern requested+overlay → dropped + warning detail; DB-miss char → carrier advance; `width_pt` consistency.
- Line height: overlaid run line height equals DB `(win_ascent+win_descent+line_gap)/upem*size` exactly.
- Emitter: `/W` equals layout advances for an overlaid run (round-trip via `pdf_core::extract` rectangles or the existing width assertions); no-TJ invariant holds.
- Fixture: `gdocs_fixtures_test.rs` asserts Ubuntu resolves with `font_metrics_overlay` (not `font_fallback`) and anchors-toc page count still 10.

## Human task H6 (with this phase)

Download Canonical's **Ubuntu Font Family 0.83** and **Ubuntu Mono 0.80** official archives (design.ubuntu.com/font — agents must not fetch binaries), run `cargo run --bin extract_metrics -- <ttf paths>`, commit the generated `metrics_db/ubuntu*.rs` modules, and record each source file's SHA-256 in the generated headers. No TTF enters the repo in this path.

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo run -p fidelity -- run --case gdocs-anchors-toc     # gate ≥ 0.90
cargo run -p fidelity -- run --format docx                # full corpus, no regressions
cd wasm && ./build.sh && cd ../.. && node --test tests/
```
