# Agent tasks 1 — office-core scaffold, OPC container, OOXML models (DOCX side)

Self-contained: you need only this file, `README.md` in this folder (envelope + DocProfile contract), and the repo. Follow existing repo conventions — `pdf-core/` is the style reference (error enums via `thiserror`, `&[u8]` in / `Vec<u8>` out, doc-comments on every public item, tests in `tests/`). Do NOT modify anything under `pdf-core/` or `pdf-core/wasm/`.

## T1.1 Crate scaffold

Create `office-core/Cargo.toml`:

```toml
[package]
name = "office-core"
version = "0.0.5"            # keep in lockstep with pdf-core's current version line
edition = "2024"
rust-version = "1.89"
description = "DocKosha proprietary Office->PDF engine: DOCX/PPTX/XLSX layout and PDF emission, pure Rust"
publish = false

[lib]
name = "office_core"

[[bin]]
name = "officecore"
path = "src/bin/officecore.rs"
required-features = ["cli"]

[features]
default = ["cli", "bundled-fonts"]
cli = ["dep:clap"]
# Embeds office-core/fonts/**.ttf via include_bytes! for zero-config native + Node wasm.
bundled-fonts = []
# wasm32 builds: JS crypto for any randomness pulled in transitively.
wasm = []

[dependencies]
clap = { version = "4", features = ["derive"], optional = true }
zip = { version = "2", default-features = false, features = ["deflate"] }
roxmltree = "0.21"
ttf-parser = "0.25"
subsetter = "0.2"
unicode-linebreak = "0.1"
unicode-bidi = "0.3"
pdf-writer = "0.15"
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
miniz_oxide = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
sha2 = "0.10"

[dev-dependencies]
serial_test = "3"
```

Pin whatever latest compatible minor exists at implementation time; if `zip` v2's API differs from expectation, prefer it anyway over v0.6 (security fixes). Every dependency here is MIT/Apache/BSD/zlib — do not add any dependency with GPL/AGPL/MPL license, and do not add any crate that binds C/C++ code.

`office-core/src/lib.rs` — the entire public surface:

```rust
pub mod error;   pub mod opc;    pub mod oxml;
pub mod fonts;   pub mod text;   pub mod layout;
pub mod pptx;    pub mod xlsx;   pub mod emit;   pub mod profile;

pub use error::{OfficeError, Result, UnsupportedCode};
pub use profile::DocProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat { Docx, Pptx, Xlsx }

impl OfficeFormat {
    /// Maps a lowercase extension. `xlsm` is Xlsx (macros never render).
    pub fn from_extension(ext: &str) -> Option<Self> { /* docx|pptx|xlsx|xlsm */ }
}

#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// Extra font directories (native only; ignored on wasm).
    pub font_dirs: Vec<std::path::PathBuf>,
}

#[derive(Debug)]
pub struct ConvertOutput {
    pub pdf: Vec<u8>,
    pub page_count: u32,
    pub warnings: Vec<profile::Warning>,
}

/// Convert an Office document to PDF. Fail-closed: anything outside the
/// documented envelope returns Err(OfficeError::Unsupported{..}).
pub fn convert(bytes: &[u8], format: OfficeFormat, opts: &ConvertOptions) -> Result<ConvertOutput>;

/// Cheap envelope check WITHOUT layout (router contract; see plan README).
pub fn profile(bytes: &[u8], format: OfficeFormat) -> Result<DocProfile>;
```

For this task, `convert()` for all formats and `profile()` may return `Err(OfficeError::Unsupported { code: UnsupportedCode::FormatNotImplemented, .. })` — later tasks fill them in. The API shape must be final now.

`office-core/src/error.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedCode {
    FormatNotImplemented, LegacyFormat, Encrypted, ComplexScript, RtlText,
    Footnotes, Endnotes, TextBox, AnchoredDrawing, FloatingTable, MultiColumn,
    Chart, SmartArt, OleObject, WordArt, Equation, FieldUnsupported,
    AutoHyphenation, ImageCrop, ConditionalFormatting, NeedsAutofitRecompute,
    UnknownElement, InputTooLarge, InvalidFile,
}

#[derive(Debug, thiserror::Error)]
pub enum OfficeError {
    #[error("unsupported: {code:?}: {detail}")]
    Unsupported { code: UnsupportedCode, detail: String },
    #[error("invalid input: {0}")] Invalid(String),
    #[error("font error: {0}")]    Font(String),
    #[error("internal: {0}")]      Internal(String),
}
pub type Result<T> = std::result::Result<T, OfficeError>;
```

Acceptance: `cargo check -p office-core` passes (run from `office-core/`); `cargo test` runs 0+ tests green.

## T1.2 OPC container — `src/opc.rs`

OOXML files are ZIP archives ("OPC packages"). Implement:

```rust
pub struct Package { /* part name (lowercased, '/'-separated, no leading '/') -> bytes */ }
impl Package {
    /// Rejects: not a zip, zip64 with > 10_000 entries, any single part > 512 MiB
    /// decompressed, total decompressed > 1 GiB (zip-bomb guards -> Invalid).
    /// An `EncryptedPackage` part or OLE CFB magic (D0 CF 11 E0) at offset 0 means
    /// an encrypted/legacy container -> Unsupported{Encrypted}/Unsupported{LegacyFormat}.
    pub fn open(bytes: &[u8]) -> Result<Package>;
    pub fn part(&self, name: &str) -> Option<&[u8]>;
    /// [Content_Types].xml: Default (by extension) + Override (by part name).
    pub fn content_type(&self, part: &str) -> Option<&str>;
    /// Parses `<dir>/_rels/<file>.rels` for a source part ("" = package root).
    /// Returns (rel id, type URI, target resolved against the source part's dir,
    /// target_mode Internal|External).
    pub fn relationships(&self, source_part: &str) -> Result<Vec<Relationship>>;
}
```

Relationship target resolution: targets are relative to the source part's directory; normalize `..` segments; external targets (hyperlinks) keep the raw URI. Main document part = the package-root relationship of type `http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument`.

Tests (`office-core/tests/opc_test.rs`): open every docx/pptx/xlsx in `../mock-files/`; assert main part found; assert a `.bin` renamed to `.docx` returns `Invalid`; assert a legacy `.doc` (CFB magic — add `mock-files/legacy-stub.doc`, any tiny real .doc) returns `Unsupported{LegacyFormat}`.

## T1.3 XML conventions — `src/oxml/mod.rs`

All parsing uses `roxmltree` (DOM; documents are size-capped by T1.2 so DOM is safe). Namespace handling: match on `(namespace_uri, local_name)` — never on prefixes. Define the URI constants once in `oxml/mod.rs`: `W` (wordprocessingml/2006/main), `A` (drawingml/2006/main), `P` (presentationml/2006/main), `S` (spreadsheetml/2006/main), `R` (officeDocument/2006/relationships), `WP` (drawingml/2006/wordprocessingDrawing), plus the strict-namespace 2010+ variants where encountered.

Shared unit helpers (also in `oxml/mod.rs`, with unit tests):
```rust
pub fn emu_to_pt(emu: i64) -> f64      { emu as f64 / 12_700.0 }      // 914_400 EMU/inch
pub fn twip_to_pt(twips: i64) -> f64   { twips as f64 / 20.0 }
pub fn half_point_to_pt(v: i64) -> f64 { v as f64 / 2.0 }             // w:sz font size
pub fn eighth_point_to_pt(v: i64) -> f64 { v as f64 / 8.0 }           // border w:sz
// table width units: w:w @w:type: "dxa"=twips, "pct"=fiftieths of a percent (5000=100%), "auto"
```

**Fail-closed walker rule (applies to every model in this plan):** when iterating children of a content element, match known elements; elements on the IGNORE list are skipped; anything else → `Err(Unsupported{UnknownElement, detail: "<w:foo> in <w:p>"})`. IGNORE list for WML (invisible bookkeeping, safe to skip): `bookmarkStart`, `bookmarkEnd`, `proofErr`, `permStart`, `permEnd`, `commentRangeStart`, `commentRangeEnd`, `commentReference` (comments don't print), `lastRenderedPageBreak`, `sectPrChange`, `pPrChange`, `rPrChange`, `tblPrChange`, `tcPrChange`, `trPrChange`, `del` (deleted tracked text — Word prints the final view), `moveFrom`, `noProof`, `webHidden`, `snapToGrid`, `w:softHyphen` (render nothing v1), any `mc:AlternateContent` — for AlternateContent take the `mc:Fallback` branch if present, else Unsupported. `w:ins` (inserted tracked text) is NOT ignored: unwrap it and render its runs normally. `w:sdt` at block/inline/row/cell level: unwrap `w:sdtContent` transparently.

## T1.4 WML model — `src/oxml/wml.rs`

Typed model of `word/document.xml` (parse only; no layout). Structs (all `#[derive(Debug, Clone)]`):

- `Document { body: Vec<BlockContent>, final_sect: SectPr }` — `BlockContent::Paragraph(Paragraph) | Table(Table)`.
- `Paragraph { props: ParaProps, runs: Vec<InlineContent>, sect_break: Option<SectPr> }` (a `w:sectPr` inside `w:pPr` ends a section).
- `ParaProps { style_id: Option<String>, alignment: Option<Jc>, spacing: Spacing, indent: Indent, tabs: Vec<TabStop>, numbering: Option<NumRef {num_id, ilvl}>, keep_next: bool, keep_lines: bool, page_break_before: bool, widow_control: Option<bool>, borders: ParaBorders, shading: Option<Rgb>, contextual_spacing: bool, outline_lvl: Option<u8> }`.
- `Spacing { before_pt, after_pt, line: LineSpacing }` with `LineSpacing::Auto(f64 /*multiple = w:line/240*/) | AtLeast(f64 /*pt*/) | Exact(f64)`; absent = `Auto(1.0)` (docDefaults may override).
- `InlineContent::Run(Run) | Hyperlink { rel_id: Option<String>, anchor: Option<String>, runs: Vec<Run> } | SimpleField(Field) | ComplexFieldRegion(Field)` — complex fields (`w:fldChar begin/separate/end` + `w:instrText`) must be assembled during parsing into one `Field { instruction: String, cached_runs: Vec<Run> }` (cached result = runs between `separate` and `end`).
- `Run { props: RunProps, content: Vec<RunContent> }`; `RunContent::Text(String) | Tab | Break(BreakKind /*line|page|column*/) | Drawing(dml::Drawing) | Symbol { font: String, char_code: u32 }`. `w:noBreakHyphen` → `Text("\u{2011}")`.
- `RunProps { style_id, bold, italic, underline: Option<UnderlineKind>, strike, dstrike, caps, small_caps, vanish, color: Option<Rgb|Auto>, highlight: Option<Rgb>, shading: Option<Rgb>, size_pt: Option<f64>, fonts: RunFonts { ascii, h_ansi, east_asia, cs }, vert_align: Option<Super|Sub>, spacing_pt: Option<f64> /*w:spacing char tracking, twips*/, rtl: bool }` — all `Option`/bool with `merge(base)` for the style cascade (T1.5).
- `Table { props: TblProps, grid: Vec<f64 /*pt*/>, rows: Vec<Row> }`, `Row { props: RowProps { height: Option<RowHeight{AtLeast|Exact}>, header: bool }, cells: Vec<Cell> }`, `Cell { props: CellProps { width: TblWidth, grid_span: u32, v_merge: Option<Restart|Continue>, borders, shading, margins, v_align }, content: Vec<BlockContent> }` (cells recurse — nested tables are in-envelope).
- `SectPr { page_w_pt, page_h_pt, orientation, margins {top,right,bottom,left,header,footer,gutter}, cols: ColumnSpec, header_refs: Vec<HdrFtrRef {kind: Default|First|Even, rel_id}>, footer_refs, title_pg: bool, page_num_start: Option<u32> }`. `ColumnSpec` with `num > 1` or non-empty `w:col` list → record it (profile/layout reject with `MultiColumn`).
- Headers/footers: parse `word/header<N>.xml` / `footer<N>.xml` via their rel ids into `Vec<BlockContent>`.

Constructs that MUST be detected here and surfaced as profile reasons (store as flags on `Document`, don't error at parse time — `profile()` needs to report them all at once): `footnoteReference`→Footnotes, `endnoteReference`→Endnotes, `w:pict`/VML or `mc:AlternateContent` without usable Fallback→UnknownElement, `m:oMath`→Equation, `w:framePr`→TextBox, anchored drawings (`wp:anchor`)→AnchoredDrawing, `tblpPr`→FloatingTable, multi-column→MultiColumn, `w:autoHyphenation` in settings→AutoHyphenation, `a:srcRect` crop on images→ImageCrop, chart/smartart rel types→Chart/SmartArt, `w:object`/OLE→OleObject.

Tests (`tests/wml_test.rs`): parse each docx in `mock-files/`; write 6+ focused fixtures into `mock-files/oxml/` (hand-crafted minimal docx zips built by a test helper in `office-core/src/testkit.rs`, mirroring pdf-core's testkit pattern): styles cascade, numbering, table with gridSpan+vMerge, complex field, sectPr with headers, tracked changes (ins+del). Assert exact parsed values.

## T1.5 Styles & docDefaults — `src/oxml/styles.rs`

Parse `word/styles.xml`: `docDefaults` (rPrDefault + pPrDefault) and each `w:style` (`type`, `styleId`, `basedOn`, `pPr`, `rPr`, `default` attr). Provide:

```rust
pub struct StyleResolver { /* id -> Style, defaults */ }
impl StyleResolver {
    /// Effective run properties: docDefaults -> default para style -> paragraph style
    /// chain (basedOn, cycle-safe: max depth 16) -> character style chain -> direct rPr.
    pub fn run_props(&self, para: &ParaProps, run: &RunProps) -> RunProps;
    /// Effective paragraph properties: docDefaults -> style chain -> direct pPr;
    /// numbering-level pPr (from numbering.rs) merges between style and direct.
    pub fn para_props(&self, para: &ParaProps) -> ParaProps;
}
```
Absent effective size = 11pt (Word's default when docDefaults omits `w:sz` — note it in a comment); absent font = the theme minorFont (from theme.rs), else "Calibri".

## T1.6 Numbering — `src/oxml/numbering.rs`

Parse `word/numbering.xml`: `abstractNum` (per `lvl`: `numFmt`, `lvlText` (patterns like `"%1."`), `start`, `suff` tab|space|nothing, `pPr` indents, `rPr`) + `num` (numId → abstractNumId + `lvlOverride`). Expose `resolve(num_id, ilvl) -> LevelDef`. Number formatting helper `format_number(fmt, n)` supporting: `decimal`, `bullet` (lvlText literal, often Symbol/Wingdings chars — see fonts task for the mapping), `lowerLetter`/`upperLetter` (a..z, aa..), `lowerRoman`/`upperRoman`, `decimalZero`. Other formats → Unsupported{UnknownElement, "numFmt:<x>"}. Counter semantics live in task 3 (layout).

## T1.7 Settings & theme — `src/oxml/settings.rs`, `src/oxml/theme.rs`

`settings.rs`: `default_tab_stop_pt` (default 36pt = 720 twips), `even_and_odd_headers: bool`, `auto_hyphenation: bool`, `compat_mode: u8` (from `w:compatSetting w:name="compatibilityMode"`, default 15; parse-only, recorded in profile stats).

`theme.rs`: parse `theme1.xml` → `clrScheme` (12 named colors; note `window`/`windowText` map to lt1/dk1) and `fontScheme` (major/minor latin typeface). Implement DrawingML color resolution used by BOTH wml (themeColor attrs) and pptx:

```rust
/// Applies DrawingML color transforms in order of appearance.
/// tint:    c' = c*t + (1-t)*255      (t = val/100_000, per-channel sRGB approximation)
/// shade:   c' = c*s                  (s = val/100_000)
/// lumMod/lumOff: convert sRGB->HSL, L' = clamp(L*lumMod + lumOff), HSL->sRGB
/// satMod:  S' = clamp(S*satMod) in HSL
/// alpha:   record opacity (0..1) alongside RGB
pub fn resolve_color(base: ColorRef, transforms: &[ColorTransform], theme: &Theme) -> Rgba;
```
Unit-test against 6 known pairs (compute expected values by hand in the test comments — e.g. accent1 4472C4 with lumMod 60000 → 2A4778-ish; derive precisely in the test).

## T1.8 CI workflow

Add `.github/workflows/office-core-ci.yml`: on push/PR touching `office-core/**` or `fidelity-corpus/**`: `cargo fmt --check`, `cargo clippy -p office-core -- -D warnings`, `cargo test -p office-core`, and `cargo check -p office-core --no-default-features --features wasm --target wasm32-unknown-unknown` (proves the engine keeps compiling for wasm from day one). Mirror the checkout/toolchain steps from the existing `publish-wasm.yml`.

## Local verification (run all, in order)

1. `cd office-core && cargo fmt --check && cargo clippy -- -D warnings`
2. `cargo test` — all green.
3. `rustup target add wasm32-unknown-unknown && cargo check --no-default-features --features wasm --target wasm32-unknown-unknown`
4. Every new public item has a doc comment (`#![deny(missing_docs)]` on the crate — add it and fix fallout).
