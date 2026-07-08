//! The DocProfile router contract — the cheap, layout-free verdict `profile()`
//! returns and the wasm `office_profile()` serializes.
//!
//! The struct shape and JSON field names are the stable v1 contract (see the
//! plan README). `profile()` never returns `Err` for a well-formed file —
//! reasons carry the verdict; `Err` is reserved for container-level failures
//! (invalid/encrypted/legacy), which the wasm boundary maps to JSON via
//! [`DocProfile::from_error`].

use crate::fonts::{FontDb, matching};
use crate::opc::Package;
use crate::oxml::numbering::{NumFmt, Numbering};
use crate::oxml::settings::Settings;
use crate::oxml::styles::StyleResolver;
use crate::oxml::theme::Theme;
use crate::oxml::wml::{
    self, BlockContent, DocFlags, InlineContent, ParaProps, Paragraph, RunContent, RunProps,
};
use crate::text::script::{ScriptScan, scan_scripts};
use crate::{OfficeError, OfficeFormat, Result, UnsupportedCode};
use std::collections::HashMap;

/// A stable, non-fatal warning about a documented-lossy conversion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Warning {
    /// The stable warning slug.
    pub code: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

impl Warning {
    /// Constructs a warning from a static code and a detail string.
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Warning {
            code,
            detail: detail.into(),
        }
    }
}

/// A single reason a document falls outside the supported envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Reason {
    /// The stable reason code.
    pub code: UnsupportedCode,
    /// Human-readable detail (e.g. the offending element name).
    pub detail: String,
}

impl Reason {
    /// Constructs a reason from a code and a detail string.
    pub fn new(code: UnsupportedCode, detail: impl Into<String>) -> Self {
        Reason {
            code,
            detail: detail.into(),
        }
    }
}

/// Cheap document statistics gathered while profiling (no layout).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Number of paragraphs (docx).
    pub paragraphs: u32,
    /// Number of tables.
    pub tables: u32,
    /// Number of raster images.
    pub images: u32,
    /// Number of slides (pptx).
    #[serde(skip_serializing_if = "is_zero")]
    pub slides: u32,
    /// Number of shapes (pptx).
    #[serde(skip_serializing_if = "is_zero")]
    pub shapes: u32,
    /// Number of sheets (xlsx).
    #[serde(skip_serializing_if = "is_zero")]
    pub sheets: u32,
    /// Distinct font families requested by the document.
    pub fonts_used: Vec<String>,
    /// Requested families with no exact or metric-compatible match.
    pub fonts_missing: Vec<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// The router contract v1 — returned by `profile()` / `office_profile()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocProfile {
    /// Always `"office-core"`.
    pub engine: &'static str,
    /// The engine crate version (`env!("CARGO_PKG_VERSION")`).
    pub engine_version: &'static str,
    /// The document format: `"docx"`, `"pptx"`, or `"xlsx"`.
    pub format: &'static str,
    /// Whether the document converts inside the validated envelope.
    pub supported: bool,
    /// Why it is unsupported (empty when `supported`).
    pub reasons: Vec<Reason>,
    /// Documented-lossy warnings that still convert.
    pub warnings: Vec<Warning>,
    /// Cheap document statistics.
    pub stats: Stats,
}

impl DocProfile {
    /// The engine version string, from the crate's `CARGO_PKG_VERSION`.
    pub const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    /// Creates a profile for `format`, seeded as supported with empty stats.
    pub fn new(format: &'static str) -> Self {
        DocProfile {
            engine: "office-core",
            engine_version: Self::VERSION,
            format,
            supported: true,
            reasons: Vec::new(),
            warnings: Vec::new(),
            stats: Stats::default(),
        }
    }

    /// Records an unsupported reason and flips `supported` to `false`.
    pub fn add_reason(&mut self, code: UnsupportedCode, detail: impl Into<String>) {
        self.supported = false;
        self.reasons.push(Reason::new(code, detail));
    }

    fn add_reason_deduped(&mut self, code: UnsupportedCode, detail: impl Into<String>) {
        let reason = Reason::new(code, detail);
        if !self.reasons.contains(&reason) {
            self.supported = false;
            self.reasons.push(reason);
        }
    }

    fn add_warning_deduped(&mut self, warning: Warning) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    /// Builds a profile from a container-level error (the wasm boundary uses
    /// this so `office_profile()` never throws for merely-unsupported input).
    pub fn from_error(format: OfficeFormat, err: &OfficeError) -> DocProfile {
        let mut profile = DocProfile::new(format_str(format));
        let (code, detail) = match err {
            OfficeError::Unsupported { code, detail } => (*code, detail.clone()),
            OfficeError::Invalid(m) => (UnsupportedCode::InvalidFile, m.clone()),
            OfficeError::Font(m) => (UnsupportedCode::UnknownElement, m.clone()),
            OfficeError::Internal(m) => (UnsupportedCode::InvalidFile, m.clone()),
        };
        profile.add_reason(code, detail);
        profile
    }
}

/// Maps a format to its stable JSON string.
fn format_str(format: OfficeFormat) -> &'static str {
    match format {
        OfficeFormat::Docx => "docx",
        OfficeFormat::Pptx => "pptx",
        OfficeFormat::Xlsx => "xlsx",
    }
}

/// Implements `profile()` (see [`crate::profile`]).
pub fn run(bytes: &[u8], format: OfficeFormat) -> Result<DocProfile> {
    match format {
        OfficeFormat::Docx => profile_docx(bytes),
        OfficeFormat::Pptx => profile_pptx(bytes),
        OfficeFormat::Xlsx => profile_xlsx(bytes),
    }
}

fn profile_xlsx(bytes: &[u8]) -> Result<DocProfile> {
    use crate::oxml::sml::CellValue;
    use crate::xlsx::XlsxDoc;

    let doc = XlsxDoc::open(bytes, &crate::ConvertOptions::default())?;
    let mut profile = DocProfile::new("xlsx");
    let mut scripts = ScriptScan::default();
    let fonts_used = vec!["Calibri".to_string()]; // the normal font

    for sheet in &doc.sheets {
        if sheet.sheet.has_drawing {
            profile.add_reason_deduped(UnsupportedCode::AnchoredDrawing, "xl/drawings on sheet");
        }
        for cell in &sheet.sheet.cells {
            match &cell.value {
                CellValue::UncachedFormula => {
                    profile.add_reason_deduped(UnsupportedCode::UnknownElement, "uncached formula")
                }
                CellValue::Text(t) => scripts.merge(scan_scripts(t)),
                _ => {}
            }
        }
    }
    if scripts.complex_script {
        profile.add_reason_deduped(UnsupportedCode::ComplexScript, "complex script text");
    }
    if scripts.rtl {
        profile.add_reason_deduped(UnsupportedCode::RtlText, "right-to-left text");
    }

    let db = FontDb::with_bundled();
    resolve_fonts(&mut profile, &fonts_used, &db, &scripts);
    profile.stats.sheets = doc.sheets.len() as u32;
    profile.stats.fonts_used = fonts_used;
    Ok(profile)
}

fn profile_pptx(bytes: &[u8]) -> Result<DocProfile> {
    use crate::oxml::pml::{Fill, ShapeElem, TextBody, TextRun};
    use crate::pptx::PptxDoc;

    let doc = PptxDoc::open(bytes, &crate::ConvertOptions::default())?;
    let mut profile = DocProfile::new("pptx");
    let mut scripts = ScriptScan::default();
    let mut fonts_used: Vec<String> = Vec::new();
    let mut shape_count = 0u32;

    fn add_font(fonts: &mut Vec<String>, theme_minor: &Option<String>, run: &TextRun) {
        let f = run
            .typeface
            .clone()
            .filter(|f| !f.starts_with('+'))
            .or_else(|| theme_minor.clone())
            .unwrap_or_else(|| "Calibri".to_string());
        if !fonts.contains(&f) {
            fonts.push(f);
        }
    }

    fn walk_body(
        body: &TextBody,
        scripts: &mut ScriptScan,
        fonts: &mut Vec<String>,
        minor: &Option<String>,
    ) {
        for para in &body.paragraphs {
            for run in &para.runs {
                scripts.merge(scan_scripts(&run.text));
                add_font(fonts, minor, run);
            }
        }
    }

    fn walk(
        elems: &[ShapeElem],
        profile: &mut DocProfile,
        scripts: &mut ScriptScan,
        fonts: &mut Vec<String>,
        minor: &Option<String>,
        shape_count: &mut u32,
    ) {
        for elem in elems {
            *shape_count += 1;
            match elem {
                ShapeElem::Shape { props, text, .. } => {
                    if matches!(props.fill, Some(Fill::LinearGradient { .. })) && props.custom_geom
                    {
                        // gradient custom geometry is documented-lossy, not fatal
                    }
                    if let Some(body) = text {
                        walk_body(body, scripts, fonts, minor);
                    }
                }
                ShapeElem::Picture { drawing, .. } => {
                    if drawing.cropped {
                        profile.add_reason_deduped(UnsupportedCode::ImageCrop, "a:srcRect");
                    }
                }
                ShapeElem::Table { table, .. } => {
                    for row in &table.rows {
                        for cell in &row.cells {
                            if let Some(b) = &cell.text {
                                walk_body(b, scripts, fonts, minor);
                            }
                        }
                    }
                }
                ShapeElem::Group { children, .. } => {
                    walk(children, profile, scripts, fonts, minor, shape_count)
                }
                ShapeElem::Unsupported(detail) => {
                    let code = match detail.as_str() {
                        "chart" => UnsupportedCode::Chart,
                        "smartart" => UnsupportedCode::SmartArt,
                        "ole" => UnsupportedCode::OleObject,
                        _ => UnsupportedCode::UnknownElement,
                    };
                    profile.add_reason_deduped(code, detail.clone());
                }
            }
        }
    }

    let minor = doc.theme.minor_latin.clone();
    for rs in &doc.slides {
        walk(
            &rs.slide.shapes,
            &mut profile,
            &mut scripts,
            &mut fonts_used,
            &minor,
            &mut shape_count,
        );
    }

    if scripts.complex_script {
        profile.add_reason_deduped(UnsupportedCode::ComplexScript, "complex script text");
    }
    if scripts.rtl {
        profile.add_reason_deduped(UnsupportedCode::RtlText, "right-to-left text");
    }

    let db = FontDb::with_bundled();
    resolve_fonts(&mut profile, &fonts_used, &db, &scripts);
    profile.stats.slides = doc.slides.len() as u32;
    profile.stats.shapes = shape_count;
    profile.stats.images = doc.images.len() as u32;
    profile.stats.fonts_used = fonts_used;
    Ok(profile)
}

fn profile_docx(bytes: &[u8]) -> Result<DocProfile> {
    // Container-level failures propagate as Err (allowed by the contract).
    let pkg = Package::open(bytes)?;
    let main = pkg.main_part()?;
    let document = parse_part(&pkg, &main, wml::parse_document)?;

    let styles = optional_part(&pkg, "word/styles.xml", StyleResolver::parse)?.unwrap_or_default();
    let numbering =
        optional_part(&pkg, "word/numbering.xml", Numbering::parse)?.unwrap_or_default();
    let settings = optional_part(&pkg, "word/settings.xml", Settings::parse)?.unwrap_or_default();
    let theme = find_theme(&pkg)?;

    let mut profile = DocProfile::new("docx");
    let mut collector = Collector::new(&styles, &numbering, &theme);

    // Body content.
    let main_rels = rel_map(&pkg, &main);
    collector.walk_blocks(&document.body, &pkg, &main_rels, &mut profile);

    // Headers/footers.
    for part in header_footer_parts(&pkg, &document) {
        if let Some(bytes) = pkg.part(&part)
            && let Ok(text) = std::str::from_utf8(bytes)
            && let Ok(blocks) = wml::parse_hdr_ftr(text)
        {
            let hf_rels = rel_map(&pkg, &part);
            collector.walk_blocks(&blocks, &pkg, &hf_rels, &mut profile);
        }
    }

    // Reasons from parse-time document flags + settings.
    add_flag_reasons(&mut profile, &document.flags);
    if settings.auto_hyphenation {
        profile.add_reason_deduped(UnsupportedCode::AutoHyphenation, "w:autoHyphenation");
    }

    // Script reasons.
    if collector.scripts.complex_script {
        profile.add_reason_deduped(UnsupportedCode::ComplexScript, "complex script text");
    }
    if collector.scripts.rtl {
        profile.add_reason_deduped(UnsupportedCode::RtlText, "right-to-left text");
    }

    // Font resolution: warnings + missing fonts (deterministic, bundled DB only).
    // Sets `profile.stats.fonts_missing` directly.
    let db = FontDb::with_bundled();
    resolve_fonts(&mut profile, &collector.fonts_used, &db, &collector.scripts);

    profile.stats.paragraphs = collector.paragraphs;
    profile.stats.tables = collector.tables;
    profile.stats.images = collector.images;
    profile.stats.fonts_used = collector.fonts_used.clone();

    Ok(profile)
}

/// Maps parse-time [`DocFlags`] onto profile reasons.
fn add_flag_reasons(profile: &mut DocProfile, flags: &DocFlags) {
    let mapping: &[(bool, UnsupportedCode, &str)] = &[
        (
            flags.footnotes,
            UnsupportedCode::Footnotes,
            "w:footnoteReference",
        ),
        (
            flags.endnotes,
            UnsupportedCode::Endnotes,
            "w:endnoteReference",
        ),
        (flags.equation, UnsupportedCode::Equation, "m:oMath"),
        (flags.text_box, UnsupportedCode::TextBox, "w:framePr"),
        (
            flags.anchored_drawing,
            UnsupportedCode::AnchoredDrawing,
            "wp:anchor",
        ),
        (
            flags.floating_table,
            UnsupportedCode::FloatingTable,
            "w:tblpPr",
        ),
        (
            flags.multi_column,
            UnsupportedCode::MultiColumn,
            "multi-column section",
        ),
        (flags.image_crop, UnsupportedCode::ImageCrop, "a:srcRect"),
        (
            flags.vml_pict,
            UnsupportedCode::UnknownElement,
            "w:pict (VML)",
        ),
        (flags.chart, UnsupportedCode::Chart, "c:chart"),
        (
            flags.smart_art,
            UnsupportedCode::SmartArt,
            "a:graphicData diagram",
        ),
        (flags.ole_object, UnsupportedCode::OleObject, "w:object"),
    ];
    for &(present, code, detail) in mapping {
        if present {
            profile.add_reason_deduped(code, detail);
        }
    }
}

/// Resolves each requested font family, accumulating warnings + missing fonts,
/// and flags CJK text that no available face covers.
fn resolve_fonts(
    profile: &mut DocProfile,
    fonts_used: &[String],
    db: &FontDb,
    scripts: &ScriptScan,
) {
    let mut missing = Vec::new();
    for family in fonts_used {
        let resolved = matching::resolve(db, family, 400, false);
        for w in resolved.warnings {
            profile.add_warning_deduped(w);
        }
        if let Some(m) = resolved.missing
            && !missing.contains(&m)
        {
            missing.push(m);
        }
    }
    // CJK with no covering face is fail-closed.
    if scripts.cjk && !db_covers_cjk(db) {
        profile.add_reason_deduped(UnsupportedCode::UnknownElement, "no font for CJK");
    }
    // Store missing on the collector via the profile stats (set by caller).
    profile.stats.fonts_missing = missing;
}

/// Whether any face in `db` maps a representative CJK ideograph.
fn db_covers_cjk(db: &FontDb) -> bool {
    db.faces().iter().any(|face| {
        face.parsed()
            .and_then(|f| f.glyph_index('\u{65E5}'))
            .is_some()
    })
}

/// A walker accumulating stats, fonts, scripts, and content-derived reasons.
struct Collector<'a> {
    styles: &'a StyleResolver,
    numbering: &'a Numbering,
    theme: &'a Theme,
    fonts_used: Vec<String>,
    scripts: ScriptScan,
    paragraphs: u32,
    tables: u32,
    images: u32,
}

impl<'a> Collector<'a> {
    fn new(styles: &'a StyleResolver, numbering: &'a Numbering, theme: &'a Theme) -> Self {
        Collector {
            styles,
            numbering,
            theme,
            fonts_used: Vec::new(),
            scripts: ScriptScan::default(),
            paragraphs: 0,
            tables: 0,
            images: 0,
        }
    }

    fn walk_blocks(
        &mut self,
        blocks: &[BlockContent],
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
    ) {
        for block in blocks {
            match block {
                BlockContent::Paragraph(p) => self.paragraph(p, pkg, rels, profile),
                BlockContent::Table(t) => {
                    self.tables += 1;
                    for row in &t.rows {
                        for cell in &row.cells {
                            self.walk_blocks(&cell.content, pkg, rels, profile);
                        }
                    }
                }
            }
        }
    }

    fn paragraph(
        &mut self,
        p: &Paragraph,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
    ) {
        self.paragraphs += 1;
        let eff_para = self.styles.para_props(&p.props);
        if let Some(numref) = eff_para.numbering
            && let Some(level) = self.numbering.resolve(numref.num_id, numref.ilvl)
            && let NumFmt::Other(v) = &level.num_fmt
        {
            profile.add_reason_deduped(UnsupportedCode::UnknownElement, format!("numFmt:{v}"));
        }
        for inline in &p.runs {
            match inline {
                InlineContent::Run(r) => self.run(&p.props, r, pkg, rels, profile),
                InlineContent::Hyperlink { runs, .. } => {
                    for r in runs {
                        self.run(&p.props, r, pkg, rels, profile);
                    }
                }
                InlineContent::SimpleField(f) | InlineContent::ComplexFieldRegion(f) => {
                    for r in &f.cached_runs {
                        self.run(&p.props, r, pkg, rels, profile);
                    }
                }
            }
        }
    }

    fn run(
        &mut self,
        para: &ParaProps,
        run: &wml::Run,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
    ) {
        let family = self.run_font(para, &run.props);
        if !self.fonts_used.contains(&family) {
            self.fonts_used.push(family);
        }
        let mut text = String::new();
        for content in &run.content {
            match content {
                RunContent::Text(t) => text.push_str(t),
                RunContent::Drawing(d) => {
                    self.images += 1;
                    if let Some(rel_id) = &d.blip_rel_id {
                        self.check_image(rel_id, pkg, rels, profile);
                    }
                }
                RunContent::Symbol { font, char_code } => {
                    if matching::map_symbol_char(font, *char_code).is_none() {
                        profile.add_reason_deduped(
                            UnsupportedCode::UnknownElement,
                            format!("symbol char {char_code:04X}"),
                        );
                    }
                }
                RunContent::Tab | RunContent::Break(_) => {}
            }
        }
        if !text.is_empty() {
            self.scripts.merge(scan_scripts(&text));
        }
    }

    /// The effective primary font family for a run (ascii → hAnsi → theme minor
    /// → Calibri).
    fn run_font(&self, para: &ParaProps, run: &RunProps) -> String {
        let eff = self.styles.run_props(para, run);
        eff.fonts
            .ascii
            .or(eff.fonts.h_ansi)
            .or_else(|| self.theme.minor_latin.clone())
            .unwrap_or_else(|| "Calibri".to_string())
    }

    /// Flags unsupported image container types (GIF/EMF/WMF/TIFF).
    fn check_image(
        &mut self,
        rel_id: &str,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
    ) {
        let Some(target) = rels.get(rel_id) else {
            return;
        };
        if let Some(ct) = pkg.content_type(target) {
            let unsupported = matches!(
                ct,
                "image/gif"
                    | "image/tiff"
                    | "image/x-emf"
                    | "image/emf"
                    | "image/x-wmf"
                    | "image/wmf"
                    | "image/x-wmf+xml"
            );
            if unsupported {
                let kind = ct.rsplit('/').next().unwrap_or("unknown");
                profile
                    .add_reason_deduped(UnsupportedCode::UnknownElement, format!("image/{kind}"));
            }
        }
    }
}

// --- Package helpers --------------------------------------------------------

fn parse_part<T>(pkg: &Package, part: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let bytes = pkg
        .part(part)
        .ok_or_else(|| OfficeError::Invalid(format!("missing part {part}")))?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid(format!("{part} is not UTF-8")))?;
    parse(text)
}

fn optional_part<T>(
    pkg: &Package,
    part: &str,
    parse: impl Fn(&str) -> Result<T>,
) -> Result<Option<T>> {
    match pkg.part(part) {
        Some(_) => parse_part(pkg, part, parse).map(Some),
        None => Ok(None),
    }
}

fn find_theme(pkg: &Package) -> Result<Theme> {
    for name in ["word/theme/theme1.xml", "ppt/theme/theme1.xml"] {
        if let Some(theme) = optional_part(pkg, name, Theme::parse)? {
            return Ok(theme);
        }
    }
    Ok(Theme::default())
}

/// Builds a `rel_id -> resolved target part` map for a source part.
fn rel_map(pkg: &Package, source_part: &str) -> HashMap<String, String> {
    pkg.relationships(source_part)
        .map(|rels| rels.into_iter().map(|r| (r.id, r.target)).collect())
        .unwrap_or_default()
}

/// The header/footer part names referenced by any section (deduped).
fn header_footer_parts(pkg: &Package, document: &wml::Document) -> Vec<String> {
    let Ok(main) = pkg.main_part() else {
        return Vec::new();
    };
    let rels = rel_map(pkg, &main);
    let sect = &document.final_sect;
    let mut parts = Vec::new();
    for hf in sect.header_refs.iter().chain(sect.footer_refs.iter()) {
        if let Some(target) = rels.get(&hf.rel_id)
            && !parts.contains(target)
        {
            parts.push(target.clone());
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{minimal_docx, wrap_document};

    fn profile_body(body: &str) -> DocProfile {
        let bytes = minimal_docx(body);
        run(&bytes, OfficeFormat::Docx).expect("profile")
    }

    #[test]
    fn plain_docx_is_supported() {
        let p = profile_body(r#"<w:p><w:r><w:t>Hello world</w:t></w:r></w:p>"#);
        assert!(p.supported, "reasons: {:?}", p.reasons);
        assert_eq!(p.format, "docx");
        assert_eq!(p.engine, "office-core");
        assert_eq!(p.stats.paragraphs, 1);
        // Serializes to camelCase JSON.
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"engineVersion\""));
        assert!(json.contains("\"fontsUsed\""));
    }

    #[test]
    fn footnotes_are_unsupported() {
        let p = profile_body(r#"<w:p><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#);
        assert!(!p.supported);
        assert!(
            p.reasons
                .iter()
                .any(|r| r.code == UnsupportedCode::Footnotes)
        );
    }

    #[test]
    fn equation_is_unsupported() {
        let p = profile_body(r#"<w:p><w:r><m:oMath/></w:r></w:p>"#);
        assert!(
            p.reasons
                .iter()
                .any(|r| r.code == UnsupportedCode::Equation)
        );
    }

    #[test]
    fn multicolumn_section_is_unsupported() {
        let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
            <w:sectPr><w:cols w:num="2"/></w:sectPr>"#;
        let p = profile_body(body);
        assert!(
            p.reasons
                .iter()
                .any(|r| r.code == UnsupportedCode::MultiColumn)
        );
    }

    #[test]
    fn arabic_text_is_complex_and_rtl() {
        let p = profile_body(r#"<w:p><w:r><w:t>مرحبا بالعالم</w:t></w:r></w:p>"#);
        assert!(
            p.reasons
                .iter()
                .any(|r| r.code == UnsupportedCode::ComplexScript)
        );
        assert!(p.reasons.iter().any(|r| r.code == UnsupportedCode::RtlText));
    }

    #[test]
    fn textbox_frame_is_unsupported() {
        let p = profile_body(
            r#"<w:p><w:pPr><w:framePr w:w="2000"/></w:pPr><w:r><w:t>tb</w:t></w:r></w:p>"#,
        );
        assert!(p.reasons.iter().any(|r| r.code == UnsupportedCode::TextBox));
    }

    #[test]
    fn all_formats_profile_without_panicking() {
        // Every format now has a profiler; a docx opened as another format just
        // yields an empty (supported) verdict rather than FormatNotImplemented.
        let bytes = minimal_docx("<w:p/>");
        for fmt in [OfficeFormat::Docx, OfficeFormat::Pptx, OfficeFormat::Xlsx] {
            let p = run(&bytes, fmt).expect("profile");
            assert_eq!(p.engine, "office-core");
        }
    }

    #[test]
    fn container_error_maps_via_from_error() {
        let err = OfficeError::unsupported(UnsupportedCode::LegacyFormat, "legacy");
        let p = DocProfile::from_error(OfficeFormat::Docx, &err);
        assert!(!p.supported);
        assert_eq!(p.reasons[0].code, UnsupportedCode::LegacyFormat);
    }

    #[test]
    fn wrap_document_helper_used() {
        // Ensures the fixture wrapper stays in sync with the parser.
        let xml = wrap_document("<w:p><w:r><w:t>ok</w:t></w:r></w:p>");
        assert!(xml.contains("<w:body>"));
    }
}
