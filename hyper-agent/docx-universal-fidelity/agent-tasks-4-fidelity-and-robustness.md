# Agent tasks 4 — fidelity gaps and robustness (all files)

Depends on: agent-tasks-1 (T4.5 additionally on the T1.0 baseline evidence); T4.1/T4.2 pair with file 1's features landing in agent-tasks-2. All work in `office-core/`. References `main` @ `4bb9efc`.

## T4.1 — Footnote/endnote markers must be sequential, not raw ids

**Bug:** the in-text reference renders the raw `w:id` (`layout/paragraph.rs` ~954, `RunContent::NoteReference { id, .. } => … id.to_string()`), and the note-body marker paragraph is built the same way (`layout/flow.rs::place_footnotes` / `append_endnotes`: `RunContent::Text(format!("{id} "))`). File 1's footnotes carry ids **0 and 1** → Word renders **1 and 2**.

**Fix:** number by first-reference order, per kind:
1. In `flow.rs::layout_docx`, after `build_units` for the whole document, build `note_seq: HashMap<(NoteKind, i64), u32>` from the order note refs first appear across units (`unit.note_refs` preserves document order; endnote collection already walks this way).
2. Thread `note_seq` (or a closure) into paragraph layout's `NoteReference` rendering and into the marker paragraphs in `place_footnotes`/`append_endnotes` — render `format_number(&NumFmt::Decimal, seq)`.
3. `w:footnotePr`/`w:endnotePr` `numFmt`/`start`/`numRestart` remain unimplemented (settings.rs currently files section-level `footnotePr` under `visible_unsupported` — leave that behavior; document-level `settings.xml` footnotePr is ignored today and stays so). Note in the code comment; H5 flywheel can add a case later.
4. Tests: two footnotes with ids 0/1 → markers "1"/"2" in both the text and the note bodies; interleaved endnote gets its own sequence; existing footnote layout tests updated.

## T4.2 — Internal hyperlink destinations (file 1's TOC links, 18×)

**Gap:** `w:hyperlink w:anchor="_…"` parses (`wml.rs::inline_level` keeps `anchor`), but layout maps only `rel_id` → `LinkTarget::Url` (`paragraph.rs` ~689); `w:bookmarkStart` is on `WML_IGNORE`, so anchors have nothing to resolve against and the link silently drops.

**Fix (three layers):**
1. **Parse:** remove `bookmarkStart`/`bookmarkEnd` from blind ignore *for inline walks only*: in `inline_level`, handle `w:bookmarkStart` → `out.push(InlineContent::Bookmark(name))` (zero-width; `w:name` attribute), ignore `bookmarkEnd` as before. Keep both on `WML_IGNORE` for contexts that skip them today (run-level, block-level between paragraphs — also capture block-level `bookmarkStart` in `block_child` by attaching to the *next* paragraph's inline stream or a `Paragraph.bookmarks: Vec<String>` field; file 1's TOC targets are heading paragraphs, where Google puts the bookmark inside the paragraph).
2. **Layout:** `LinkTarget` gains `Anchor(String)`; hyperlinks with `anchor` produce it (`paragraph.rs` link-target resolution: `rel_id → Url`, else `anchor → Anchor`). During pagination, record `bookmark_positions: HashMap<String, (page: u32, y_pt: f64)>` when the owning unit is placed (same mechanics as `collect_outline`; a `LineItem`-adjacent zero-width marker item, or carry bookmark names on the unit like `outline`).
3. **Emit:** `emit/pdf.rs` link-annotation writer currently handles `Url` and `Page` — resolve `Anchor(name)` through `bookmark_positions` (pass the map on `LaidOutDocument`, e.g. `pub anchors: Vec<(String, u32, f64)>`) into the same GoTo/XYZ destination form used for `Page`+outline destinations. An unresolved anchor renders text with **no** annotation (matches Word's behavior for dangling refs; not an error).
4. Tests: bookmark on page 2 heading + anchor hyperlink on page 1 → link annotation with a page-2 destination (assert via the existing PDF-object inspection pattern used by emit tests, or pdf-core native extract in the fidelity dev-deps); dangling anchor → no annotation, converts fine; profile unaffected (no new reasons).

## T4.3 — Tab leaders (TOC dotted lines)

File 1's TOC entries use right tab stops with `w:leader="dot"` (17 tab-stop definitions). `parse_tabs` already parses `TabLeader::{Dot,Hyphen,Underscore}`.

1. Verify whether `paragraph.rs` paints leaders when advancing to a tab stop (search for `TabLeader` uses beyond parsing). The committed `tabs-leaders` corpus case exercises *stops*; confirm whether any case asserts leader glyphs.
2. If unpainted: implement — fill the tab advance with the leader character repeated at the tab-context font/size, right-aligned so the fill ends flush at the stop (Word metric: repeat char advance, floor count, min 0). Paint as a normal `GlyphRun` (extraction-visible, like Word).
3. Test: right-stop with dot leader at 8640 twips → glyph run of `.` between text end and stop; SSIM effect visible in `gdocs-anchors-toc` at H3.

## T4.4 — Bullet-glyph substitution ladder (Google's exotic bullets)

File 1's numbering defines bullets `●` U+25CF, `o`, `▪` U+25AA (used — all covered by Liberation/WGL4) and `⮚` U+2B9A, `♦` U+2666 at deeper levels (unused *in this file*, but any Google list that indents 3+ levels hits them; `♦` is WGL4-covered, `⮚` is not).

1. In the bullet path only (numbering `NumFmt::Bullet` label shaping in `layout/numbering_layout.rs::build_label` and the mirrored coverage check in `profile.rs::Collector::paragraph`), when a bullet codepoint has **no coverage across `coverage_fallbacks`**, substitute via a fixed ladder before failing: `char → ladder entry → '•' U+2022`; ladder (extend as needed, keep ≤ a dozen, comment each): `⮚/➢/➤ → ► U+25BA`, `❖ → ♦ U+2666`, `✦/✧ → ◆/◇`, `∙/· → •`. Every substitution emits `Warning { code: "bullet_substituted", detail: "U+2B9A → U+25BA" }` in both profile and convert outputs.
2. Only if the final ladder step is also uncovered → keep today's `missing_glyph` fail-closed reason.
3. `map_symbol_char` (Symbol/Wingdings mapping) runs FIRST, unchanged — the ladder is a post-mapping net.
4. Tests: a numbering level with `lvlText` `⮚` and font `Noto Sans Symbols` → converts, label renders `►`, warning emitted, profile==convert; a genuinely unmappable PUA bullet with no coverage → still `missing_glyph`.

## T4.5 — Superscript/subscript codepoints (file 2's `10⁻⁹`) — CONDITIONAL on T1.0

File 2's body text contains U+2076 ⁶, U+2079 ⁹, U+207B ⁻ (plus Latin-1 ²³µ¥ which Carlito/Liberation cover). If T1.0's baseline shows `missing_glyph` for the U+207x characters:

1. Implement **synthesis**: at the coverage/shaping boundary (`text/shape.rs` + the fallback-coverage check in `profile.rs::add_fallback_face_missing_reason` — both must agree), when no face covers a codepoint in the Superscripts/Subscripts block (U+2070–U+209C), decompose to the base ASCII digit/sign (`⁶→6`, `⁻→-`, `₄→4`) rendered with the existing script transform (`paragraph.rs` constants `SCRIPT_SCALE` 0.65 / `SCRIPT_RISE` 0.35; subscripts use the negative drop already used by `VertAlign::Sub`).
2. **ToUnicode must keep the original codepoint** — `PlacedGlyph.ch` stays U+2076 etc. so extraction/copy yields the true character (the emitter's ToUnicode writes `ch`; verify with the round-trip test).
3. Emit `Warning { code: "superscript_synthesized", detail: "U+2076" }`.
4. If instead T1.0 shows the bundled faces cover them, this task reduces to: a regression test pinning that coverage (shape U+2070–209C against the bundled set; assert no `MissingGlyphs`), so a future font swap cannot silently reintroduce the failure.
5. Tests either way; plus profile/convert parity on a fixture containing `10⁻⁹ m`.

## T4.6 — `core_dates` part-name case bug

`layout/assemble.rs::core_dates` reads `pkg.part("docprops/core.xml")` while the OPC part (file 4, every Word/Google package) is `docProps/core.xml`. Check `opc.rs::Package::part`'s lookup semantics (OPC part names are case-insensitive by spec; zip entry names are case-sensitive in practice):
- if lookups are case-sensitive: dates never load and every output uses the fixed literal — fix the string to `"docProps/core.xml"` and add a test that a package with `docProps/core.xml` yields its real `D:` dates (determinism note: dates come from *input* metadata, so output remains input-deterministic);
- if normalized: add the test anyway and a comment; no code change.

## T4.7 — Explicit-toggle audit matrix

Google writes every toggle explicitly (`w:val="0"`/`"1"`); Word omits absent ones. One table-driven unit test (in `tests/wml_test.rs`) sweeping `{absent, "1", "true", "0", "false", "off"}` across: run props `b/i/caps/smallCaps/strike/dstrike/vanish/rtl`, para props `keepNext/keepLines/pageBreakBefore/widowControl/bidi/contextualSpacing`, row props `tblHeader/cantSplit/hidden`, `sectPr titlePg` — asserting parsed values match `on_off` semantics (`oxml/mod.rs::on_off`: absent→true when element present, `0/false/off`→false, else true). This pins the semantics the three fixtures rely on (their `w:val="0"` storms must remain no-ops).

## T4.8 — Documented-lossy warnings surface end-to-end

New warning codes: `bullet_substituted`, `superscript_synthesized` (T4.5 path), `floating_table_block_fallback` (T2.3). Verify each flows through BOTH channels: `DocProfile.warnings` (router-visible) and `ConvertOutput.warnings`. Add one test per code asserting presence in both, and dedup via the existing `add_warning_deduped`. Update the DocProfile docs table in `hyper-agent/office-engine/README.md`? — **No**: that plan folder is frozen history; document new codes in `office-core/README.md` instead (task 5.5).

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
target/release/officecore convert 1.docx -o /tmp/1.pdf   # footnote markers now 1/2; TOC entries dotted + linked
target/release/officecore convert 2.docx -o /tmp/2.pdf   # superscripts render (synthesized or covered)
cargo run -p fidelity -- run --format docx               # existing cases green; heatmaps for gdocs-* improve
cd wasm && ./build.sh && cd ../.. && node --test tests/
```
