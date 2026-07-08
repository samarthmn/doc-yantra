# Plan: office-engine — DocYantra's own Office→PDF engine (Tier A)

**Repos in scope**
- `samarthmn/doc-yantra` (this repo) — the engine: new `office-core/` crate + `office-core/wasm/` npm package + fidelity harness. Plan pushed to `main`.
- `Sublime-Innovation-Technologies/doc-kosha` — integration: router in the conversion path, telemetry, rollout. Plan on branch `hyper-agent/office-engine` (cut from `staging`), see that repo's `hyper-agent/office-engine/`.

**Decision this plan implements (locked in brainstorm, 2026-07-08):** Tier A — a proprietary, pure-Rust Office→PDF engine. Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, no Chromium, no pdfium, no Typst in the engine.** Only small, permissive (MIT/Apache/OFL), pure-Rust leaf crates. The existing DocYantra LibreOffice service remains as a **routed fallback** during transition (strangler pattern); DocKosha routes each document to the engine only when the engine itself declares the document inside its supported envelope.

## Why

- Office→PDF is DocKosha's last heavyweight dependency (Gotenberg = LibreOffice+Chromium on Cloud Run: 0.5–3s conversions, 15–30s cold starts, non-deterministic output across LO versions).
- Measured precedent in this repo: pdf-core replaced Chromium/pdf-lib/PyMuPDF paths at 100–600× speedups (csv→pdf ~5ms vs 1–3s).
- Market gap (researched 2026-07): no production Rust office→PDF engine exists; only Apryse/Nutrient sell client-side WASM office conversion, at $20k+/yr. A tens-of-MB permissive WASM office converter is unoccupied territory.
- Fidelity ceiling: LibreOffice lays out like Writer, not Word. Diffing our engine against **Word's own exports** lets us exceed LO fidelity on the business-document envelope (fonts, tables, headers/footers) — LO structurally never can.

## Architecture (target state)

```
office-core/                     new standalone crate (mirrors pdf-core conventions)
├── Cargo.toml                   crate: office-core, lib: office_core, bin: officecore
├── src/
│   ├── lib.rs                   public API: convert(), profile(), errors
│   ├── error.rs                 OfficeError { Unsupported{code,detail}, Invalid, Font, Internal }
│   ├── opc.rs                   ZIP/OPC container, [Content_Types].xml, relationships
│   ├── oxml/                    typed OOXML models (parse only, no layout)
│   │   ├── wml.rs               WordprocessingML: document/body/p/r/tbl/sectPr…
│   │   ├── styles.rs            styles.xml + docDefaults + inheritance resolution
│   │   ├── numbering.rs         numbering.xml: abstractNum/num/lvl
│   │   ├── settings.rs          settings.xml: defaultTabStop, evenAndOddHeaders, autoHyphenation…
│   │   ├── theme.rs             theme1.xml: clrScheme, fontScheme + color transforms
│   │   ├── dml.rs               shared DrawingML: inline/anchor, extents, blipFill, preset geometry
│   │   ├── pml.rs               PresentationML: presentation/master/layout/slide/placeholders
│   │   └── sml.rs               SpreadsheetML: workbook/sheets/sharedStrings/styles/numFmts
│   ├── fonts/                   db.rs, matching.rs, metrics.rs, subset.rs, odttf.rs, bundled.rs
│   ├── text/                    shape.rs (cmap+kern+GPOS pairs), linebreak.rs (UAX-14), script.rs
│   ├── layout/                  THE moat — docx: page.rs, paragraph.rs, table.rs, flow.rs,
│   │                            objects.rs, numbering_layout.rs, fields.rs
│   ├── pptx/                    slide layout: inherit.rs, geometry.rs, textbody.rs, render.rs
│   ├── xlsx/                    print model: numfmt.rs, widths.rs, paginate.rs, render.rs
│   ├── emit/                    pdf.rs — pdf-writer emission (Type0/CID fonts, images, links)
│   ├── profile.rs               DocProfile extraction (router contract) — no layout needed
│   └── bin/officecore.rs        CLI: convert / profile
├── fonts/                       vendored OFL fonts (Liberation, Carlito, Caladea) + licenses
├── fidelity/                    dev-only harness crate (native; uses pdf-core's pdfium raster)
├── tests/                       integration tests per module
└── wasm/                        npm package @samarthmn/doc-yantra-office (mirrors pdf-core/wasm)
fidelity-corpus/                 repo-root corpus: cases + ground-truth renders + manifests
```

Existing `pdf-core/` is **not modified** by this plan (the harness consumes it read-only as a path dev-dependency for rasterization). `mock-files/` gains new fixtures.

## The two contracts everything hangs on

**1. Fail-closed envelope.** `convert()` returns `Err(Unsupported{code})` for any construct outside the documented envelope — never a silently-degraded page. `profile()` predicts the same verdict *without* layout, cheaply, so the router can decide before converting. Ignorable constructs (invisible bookkeeping) and documented-lossy constructs (warnings) are explicitly enumerated in each format's task file; **everything not enumerated is Unsupported**. Same philosophy as pdf-core's redaction.

**2. DocProfile JSON (router contract v1)** — returned by `profile()` and the wasm `office_profile()`:
```json
{
  "engine": "office-core",
  "engineVersion": "0.0.x",
  "format": "docx",
  "supported": true,
  "reasons": [],
  "warnings": [{"code": "font_substituted", "detail": "Calibri -> Carlito"}],
  "stats": { "paragraphs": 120, "tables": 3, "images": 5,
             "fontsUsed": ["Calibri"], "fontsMissing": [] }
}
```
`reasons[].code` values (stable strings; add, never rename): `format_not_implemented`, `legacy_format`, `encrypted`, `complex_script`, `rtl_text`, `footnotes`, `endnotes`, `text_box`, `anchored_drawing`, `floating_table`, `multi_column`, `chart`, `smart_art`, `ole_object`, `word_art`, `equation`, `field_unsupported`, `auto_hyphenation`, `image_crop`, `conditional_formatting`, `needs_autofit_recompute`, `unknown_element`, `input_too_large`, `invalid_file`.

## Task files (execution order)

| # | File | Delivers | Depends on |
|---|---|---|---|
| 1 | `agent-tasks-1-scaffold.md` | office-core crate, opc, oxml (wml/styles/numbering/settings/theme), CI | — |
| 2 | `agent-tasks-2-fonts-text-profile.md` | fonts (db/match/metrics/subset/odttf/bundled), text (shape/linebreak/script), profile.rs | 1 |
| 3 | `agent-tasks-3-docx-layout-emit.md` | DOCX layout engine + PDF emission + CLI + determinism | 2 |
| 4 | `agent-tasks-4-fidelity-harness.md` | fidelity crate, corpus layout, SSIM scoring, HTML reports, calibration loop | 3 (skeleton can start after 1) |
| 5 | `agent-tasks-5-pptx.md` | PPTX: inheritance, preset-geometry evaluator, fills, text bodies, tables | 2, 4 |
| 6 | `agent-tasks-6-xlsx.md` | XLSX: numfmt engine, MDW widths, print-model pagination | 2, 4 (parallel with 5) |
| 7 | `agent-tasks-7-packaging.md` | wasm npm package, publish workflow + release.sh extension, docs | 3 (docx-only release), full after 5+6 |

`human-tasks.md` interleaves: toolchain setup before task 1; Word ground-truth generation before task 4 scoring; corpus review gates after tasks 3/5/6; releases after 7.

**Cross-repo coordination:** doc-kosha integration (see that repo's plan) needs the **first published `@samarthmn/doc-yantra-office` 0.0.x** (docx-only is fine — `profile()` returns `format_not_implemented` for pptx/xlsx and the router falls back, so DocKosha can integrate after task 7's first release and pick up pptx/xlsx by bumping the pin). Watermark JSON parity, existing `@samarthmn/doc-yantra` package, and the LibreOffice service are untouched.

## Out of scope (explicit)

- Legacy binary formats (`.doc`/`.ppt`/`.xls`) — profile returns `legacy_format`; permanently routed to the LibreOffice service until a separate decision.
- Arbitrary HTML→PDF — stays on Chromium (service); not an engine concern.
- Charts (`c:chart`), SmartArt, WordArt, OLE, equations — `Unsupported` v1 (fallback), future milestones.
- Complex-script shaping (Arabic/Indic/Thai) and RTL — `Unsupported` v1; revisit with `harfrust` later.
- PDF/A output, N-API native addon — deferred (see task 7 notes).
- pdf-core changes of any kind.

## Verification that cannot run in the planning sandbox

Rust toolchain, wasm-pack, and pdfium are unavailable where this plan was authored. Every task file ends with the local verification list (`cargo test`, `cargo clippy`, `wasm/build.sh`, `node --test tests/`, fidelity runs). Implementers MUST run them locally; CI (added in task 1) enforces on push.
