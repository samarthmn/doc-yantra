# Agent tasks 2 — fonts, text measurement, and the DocProfile router contract

Prereq: tasks 1 complete; human task H2 done (OFL fonts committed under `office-core/fonts/`). Everything stays inside `office-core/`.

## T2.1 Font database — `src/fonts/db.rs`

```rust
pub struct FontDb { /* faces: Vec<Face { family_name, subfamily, weight(u16), italic, data: Arc<Vec<u8>>, index(u32 for TTC) }> */ }
impl FontDb {
    /// bundled-fonts feature: include_bytes! every TTF under office-core/fonts/**.
    pub fn with_bundled() -> FontDb;
    /// Native only (cfg(not(target_arch="wasm32"))): also scan directories
    /// (non-recursive is fine): /usr/share/fonts, /usr/local/share/fonts,
    /// ~/Library/Fonts, /Library/Fonts, C:\Windows\Fonts, plus opts.font_dirs.
    pub fn with_system(extra_dirs: &[PathBuf]) -> FontDb;
    /// Runtime registration (wasm `register_font` uses this).
    pub fn register(&mut self, bytes: Vec<u8>) -> Result<()>;
    /// Fonts embedded in the document itself (see T2.2) get highest priority.
    pub fn register_embedded(&mut self, family_hint: &str, bytes: Vec<u8>, bold: bool, italic: bool);
}
```
Family/weight/italic are read via `ttf-parser` (name table id 1/2 + OS/2 usWeightClass + head.macStyle italic bit). Parse failures on a system font are skipped silently; on an embedded/registered font they are `OfficeError::Font`.

## T2.2 Embedded fonts + ODTTF deobfuscation — `src/fonts/odttf.rs`

DOCX embeds fonts via `word/fontTable.xml` (`w:font` → `w:embedRegular|embedBold|embedItalic|embedBoldItalic` with `r:id` + `w:fontKey="{GUID}"`) pointing at `word/fonts/font<N>.odttf` parts. PPTX equivalent: `p:embeddedFontLst` (rel type `.../font`). ODTTF = a TTF whose **first 32 bytes are XOR-obfuscated with the GUID bytes**:

1. Strip `{}`/`-` from fontKey; parse the 32 hex chars into 16 bytes **in GUID byte order** (first three groups little-endian: bytes 3,2,1,0, 5,4, 7,6, then 8..15 as-is).
2. Build the 16-byte key by **reversing** those GUID bytes.
3. XOR input bytes 0..32 with `key[i % 16]`.
4. Validate: resulting bytes 0..4 must be `00 01 00 00` (TrueType) or `4F 54 54 4F` ("OTTO"); otherwise try the alternate interpretation (hex-string bytes reversed WITHOUT the little-endian group swap) before failing — real-world producers disagree; accept whichever yields a valid sfnt magic. Unit-test with one obfuscated fixture you generate in the test itself (take any bundled TTF, XOR its first 32 bytes with a known GUID, then assert round-trip).

Embedded fonts register with priority over everything (they are the author's exact glyphs — the single biggest fidelity win over LibreOffice defaults).

## T2.3 Matching & substitution — `src/fonts/matching.rs`

Resolution order for a requested `(family, weight, italic)`:
1. Embedded (document) fonts.
2. Exact family in FontDb (case-insensitive; also match name-table Full/Typographic names).
3. **Metric-compatible substitution map** (hardcoded, with a `substituted` warning): Calibri→Carlito, Cambria→Caladea, Arial/Helvetica→Liberation Sans, Times New Roman/Times→Liberation Serif, Courier New/Courier→Liberation Mono, Calibri Light→Carlito, Segoe UI→Liberation Sans (approx — still warn), Symbol/Wingdings/Wingdings 2/Webdings→special: map the codepoint instead (T2.4).
4. Family-class fallback by name heuristic (contains "mono"→Liberation Mono; serif-ish names ("Times","Georgia","Garamond","Book")→Liberation Serif; else Liberation Sans) with a `font_fallback` warning + `fontsMissing` entry in the profile.
Weight/style: nearest weight (700 for bold), synthetic italic/bold are NOT applied v1 — pick the closest real face and warn `synthetic_style_unavailable` if italic requested but only regular exists.

## T2.4 Symbol-font bullets — in `matching.rs`

Numbering bullets commonly use Symbol/Wingdings codepoints in the U+F000 PUA range. Map at char level (before shaping): `F0B7`(Symbol •)→U+2022, `F0A7`(Wingdings ▪)→U+25AA, `F0D8`(Wingdings ➢)→U+27A2 (render as U+25B8 if missing), `F0FC`(Wingdings ✓)→U+2713, `F06E`(Wingdings ■)→U+25A0, `F075`(Wingdings ◆)→U+25C6, `F0A8`(Wingdings □)→U+25A1; also the same codes without the F0 offset (some producers store `B7` etc.). Unmapped PUA/symbol chars → Unsupported{UnknownElement, "symbol char F0xx"} at profile time (bullets outside this table are rare; fail closed).

## T2.5 Metrics & shaping-lite — `src/fonts/metrics.rs`, `src/text/shape.rs`

`metrics.rs` (per resolved face, via `ttf-parser`):
- `units_per_em`, `ascent_pt(size)`, `descent_pt(size)`, `line_gap_pt(size)`. **Use OS/2 `usWinAscent`/`usWinDescent` when present (this is what Word uses for line height), falling back to hhea.** Document this in a comment; it is the #1 line-height parity lever.
- `advance_pt(glyph_id, size)`; `glyph_id(char) -> Option<GlyphId>` via cmap (format 4 + 12).
- Kerning: `kern` table pairs AND GPOS PairAdjustment lookups from the `kern` feature (ttf-parser exposes both via `tables().kern` / `tables().gpos`; for GPOS implement only lookup type 2 format 1/2 pair adjustment, single-subtable scan — matches what docxide proved sufficient).

`text/shape.rs`:
```rust
pub struct ShapedRun { pub glyphs: Vec<Glyph { id, advance_pt, cluster /*byte index*/ }>, pub width_pt: f64 }
/// 1:1 char->glyph mapping + kern/GPOS pair adjustments + char-spacing (w:spacing) added
/// per glyph. Per-char font fallback: if the resolved face lacks a glyph, retry the
/// fallback chain for THAT char (CJK chars will typically miss; v1 fallback chain has no
/// CJK face, so a missing glyph after fallback -> collect into `missing_glyphs`).
pub fn shape(text: &str, face: &Face, size_pt: f64, char_spacing_pt: f64, db: &FontDb) -> ShapedRun;
```

`text/script.rs` — the complex-script gate (fail-closed): scan text for codepoints in ranges Arabic (0600–06FF, 0750–077F, FB50–FDFF, FE70–FEFF), Hebrew (0590–05FF), Devanagari + other Indic (0900–0DFF), Thai/Lao (0E00–0EFF), Khmer (1780–17FF), Myanmar (1000–109F). Any hit → profile reason `ComplexScript` (Hebrew/Arabic additionally `RtlText`). CJK (4E00–9FFF, 3040–30FF, AC00–D7AF, 3000–303F, FF00–FFEF) is ALLOWED (1:1 mapping is correct) — but if no available face covers the chars, `fontsMissing` + Unsupported{UnknownElement, "no font for CJK"} at profile time. `unicode-bidi` is used only to DETECT RTL presence (`BidiInfo` paragraph level or any RTL char) — no reordering is implemented v1.

`text/linebreak.rs`: wrap `unicode-linebreak` to yield break opportunities (mandatory + allowed) by byte index; trailing spaces at a break do not count toward line width (Word behavior).

## T2.6 Subsetting & CID embedding prep — `src/fonts/subset.rs`

```rust
/// Subsets `face` to exactly the used glyph ids via the `subsetter` crate and returns
/// (font_file_bytes, cid_to_gid identity note, widths: Vec<(GlyphId, advance_1000em)>,
///  to_unicode: Vec<(GlyphId, String)>).
pub fn subset_for_pdf(face: &Face, used: &BTreeSet<GlyphId>, used_chars: &BTreeMap<GlyphId, char>) -> Result<SubsetFont>;
```
Glyph ids are remapped by `subsetter`; keep the old→new mapping so the emitter writes CIDs = new gids (Identity-H) and a ToUnicode CMap from `used_chars`. CFF-flavored (OTTO) faces: pass through `subsetter` the same way; if it rejects, fall back to embedding the full font file (warn `font_not_subset` — size cost, not a correctness cost).

## T2.7 DocProfile — `src/profile.rs`

Implements `profile(bytes, format)` per the JSON contract in this plan's README:

- Runs T1 parsing + T2 font resolution + script detection but NO layout.
- Docx flags→reason mapping is defined in T1.4; add font-based reasons from T2.5.
- `serde::Serialize` the struct with camelCase field names exactly as the README JSON shows; `engineVersion` = `env!("CARGO_PKG_VERSION")`.
- `Warning { code: &'static str, detail: String }` codes so far: `font_substituted`, `font_fallback`, `synthetic_style_unavailable`, `font_not_subset`, `tracked_changes_final_view`, `comments_stripped`.
- Pptx/Xlsx: until tasks 5/6 land, return `supported:false, reasons:[format_not_implemented]` (still parse nothing — cheap).
- `profile()` itself must never return `Err` for a well-formed file — reasons carry the verdict; `Err` only for `InvalidFile`/`Encrypted`/`LegacyFormat`/`InputTooLarge` container-level failures, and those map to the same JSON via a helper `DocProfile::from_error()` used by the wasm boundary.

Golden tests (`tests/profile_test.rs`): for each fixture from T1.4 assert the full serialized JSON (insta-style string compare without adding a snapshot dependency — plain `assert_eq!` against an expected string constant). One fixture per Unsupported code reachable for docx.

## Local verification

1. `cargo test -p office-core` (all T1 + T2 tests green).
2. `cargo clippy -p office-core -- -D warnings && cargo fmt --check`.
3. wasm check still passes: `cargo check -p office-core --no-default-features --features wasm --target wasm32-unknown-unknown` (fonts module must compile without the system-scan path — verify the cfg gates).
4. `cargo test -p office-core --no-default-features --features bundled-fonts` (bundled DB loads all 20 faces; matching resolves Calibri→Carlito with warning).
