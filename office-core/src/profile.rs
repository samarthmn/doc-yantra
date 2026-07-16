//! The DocProfile router contract — the cheap, layout-free verdict `profile()`
//! returns and the wasm `office_profile()` serializes.
//!
//! The struct shape and JSON field names are the stable v1 contract (see the
//! plan README). `profile()` never returns `Err` for a well-formed file —
//! reasons carry the verdict; `Err` is reserved for container-level failures
//! (invalid/encrypted/legacy), which the wasm boundary maps to JSON via
//! [`DocProfile::from_error`].

use crate::drawing::diagram::DiagramStore;
use crate::fonts::{
    FontDb, FontExactness, FontRequest, FontResolution, FontResolutionEvent, FontResolutionSource,
    FontSlot, SelectedFontIdentity, matching,
};
use crate::opc::Package;
use crate::oxml::font_table::FontTable;
use crate::oxml::numbering::{NumFmt, Numbering};
use crate::oxml::settings::Settings;
use crate::oxml::styles::StyleResolver;
use crate::oxml::table_style;
use crate::oxml::theme::Theme;
use crate::oxml::wml::{
    self, BlockContent, DocFlags, InlineContent, ParaProps, Paragraph, RunContent, RunProps,
    SectPr, SectType,
};
use crate::text::script::{ScriptScan, scan_scripts};
use crate::{OfficeError, OfficeFormat, Result, UnsupportedCode};
use std::collections::{BTreeSet, HashMap};

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
    /// Per-request font resolution and reproducibility telemetry.
    pub fonts: Vec<FontResolution>,
    /// Cheap document statistics.
    pub stats: Stats,
    #[serde(skip)]
    pub(crate) font_requests: Vec<FontRequest>,
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
            fonts: Vec::new(),
            stats: Stats::default(),
            font_requests: Vec::new(),
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
    run_with_options(bytes, format, &crate::ConvertOptions::default())
}

pub(crate) fn run_with_options(
    bytes: &[u8],
    format: OfficeFormat,
    opts: &crate::ConvertOptions,
) -> Result<DocProfile> {
    match format {
        OfficeFormat::Docx => profile_docx(bytes, opts),
        OfficeFormat::Pptx => profile_pptx(bytes, opts),
        OfficeFormat::Xlsx => profile_xlsx(bytes, opts),
    }
}

fn profile_xlsx(bytes: &[u8], opts: &crate::ConvertOptions) -> Result<DocProfile> {
    use crate::oxml::sml::CellValue;
    use crate::xlsx::XlsxDoc;

    let doc = XlsxDoc::open(bytes, opts)?;
    let mut profile = DocProfile::new("xlsx");
    let mut scripts = ScriptScan::default();
    let mut coverage = Vec::new();
    let mut fonts_used: Vec<String> = Vec::new();

    for sheet in &doc.sheets {
        for reason in &sheet.sheet.visible_unsupported {
            profile.add_reason_deduped(reason.code, reason.detail.clone());
        }
        for format in &sheet.sheet.conditional_formats {
            for rule in &format.rules {
                if matches!(
                    rule.kind,
                    crate::oxml::sml::ConditionalKind::Unsupported(_)
                        | crate::oxml::sml::ConditionalKind::IconSet
                ) {
                    profile.add_reason_deduped(
                        UnsupportedCode::ConditionalFormatting,
                        format!("sheet {}: unsupported conditional-format rule", sheet.name),
                    );
                }
                if let Some(dxf) = rule.dxf_id.and_then(|id| doc.styles.dxf(id)) {
                    for detail in &dxf.unsupported {
                        profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
                    }
                    for edge in [
                        dxf.borders.top.as_ref(),
                        dxf.borders.right.as_ref(),
                        dxf.borders.bottom.as_ref(),
                        dxf.borders.left.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if let Some(detail) = &edge.unsupported {
                            profile.add_reason_deduped(
                                UnsupportedCode::UnknownElement,
                                detail.clone(),
                            );
                        }
                    }
                }
            }
        }
        for reason in &sheet.drawing_reasons {
            profile.add_reason_deduped(reason.code, reason.detail.clone());
        }
        for cell in &sheet.sheet.cells {
            let style = doc.styles.cell(cell.style_idx);
            for detail in &style.unsupported {
                profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
            }
            for edge in [
                style.borders.top.as_ref(),
                style.borders.right.as_ref(),
                style.borders.bottom.as_ref(),
                style.borders.left.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                if let Some(detail) = &edge.unsupported {
                    profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
                }
            }
            match &cell.value {
                CellValue::UncachedFormula => {
                    profile.add_reason_deduped(
                        UnsupportedCode::FormulaCacheMissing,
                        format!(
                            "sheet {}: cell {}{} has no usable formula cache",
                            sheet.name,
                            xlsx_column_name(cell.col),
                            cell.row + 1
                        ),
                    );
                }
                CellValue::Text(t) => {
                    scripts.merge(scan_scripts(t));
                    let family = style.font_name.as_deref().unwrap_or("Calibri");
                    if !fonts_used
                        .iter()
                        .any(|used| used.eq_ignore_ascii_case(family))
                    {
                        fonts_used.push(family.to_string());
                    }
                    record_coverage(
                        &mut coverage,
                        family,
                        if style.bold { 700 } else { 400 },
                        style.italic,
                        t,
                    );
                }
                _ => {}
            }
        }
        for source in [
            sheet.sheet.header_footer.odd_header.as_deref(),
            sheet.sheet.header_footer.odd_footer.as_deref(),
            sheet.sheet.header_footer.even_header.as_deref(),
            sheet.sheet.header_footer.even_footer.as_deref(),
            sheet.sheet.header_footer.first_header.as_deref(),
            sheet.sheet.header_footer.first_footer.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            for span in crate::xlsx::render::header_footer_coverage_spans(source, &sheet.name) {
                scripts.merge(scan_scripts(&span.text));
                record_coverage(
                    &mut coverage,
                    "Calibri",
                    if span.bold { 700 } else { 400 },
                    span.italic,
                    &span.text,
                );
            }
            if !fonts_used
                .iter()
                .any(|used| used.eq_ignore_ascii_case("Calibri"))
            {
                fonts_used.push("Calibri".to_string());
            }
        }
    }
    if profile.supported
        && doc.sheets.iter().any(|sheet| !sheet.drawings.is_empty())
        && let Err(error) = crate::xlsx::render_xlsx(&doc)
    {
        match error {
            OfficeError::Unsupported { code, detail } => profile.add_reason_deduped(code, detail),
            OfficeError::Font(detail) => {
                profile.add_reason_deduped(UnsupportedCode::MissingGlyph, detail)
            }
            other => return Err(other),
        }
    }
    resolve_fonts(&mut profile, &coverage, &doc.fonts, &scripts);
    profile.fonts = font_resolution_report(&coverage, &doc.fonts);
    add_fallback_face_missing_reason(&mut profile, &doc.fonts, &coverage);
    profile.stats.sheets = doc.sheets.len() as u32;
    profile.stats.images = doc.images.len() as u32;
    if fonts_used.is_empty() {
        fonts_used.push("Calibri".to_string());
    }
    profile.font_requests = canonical_font_requests(&coverage, None, &doc.fonts);
    profile.stats.fonts_used = fonts_used;
    Ok(profile)
}

fn xlsx_column_name(mut col: u32) -> String {
    let mut chars = Vec::new();
    loop {
        chars.push((b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    chars.into_iter().rev().collect()
}

fn profile_pptx(bytes: &[u8], opts: &crate::ConvertOptions) -> Result<DocProfile> {
    use crate::oxml::pml::{ShapeElem, TextBody, TextRun};
    use crate::pptx::PptxDoc;
    use crate::pptx::assemble::Category;
    use crate::pptx::geometry::{evaluate_shape, supports_preset};
    use crate::pptx::inherit::resolve_text_body;

    let doc = PptxDoc::open(bytes, opts)?;
    let mut profile = DocProfile::new("pptx");
    struct TextInventory {
        scripts: ScriptScan,
        fonts: Vec<String>,
        coverage: Vec<CoverageRequest>,
    }
    let mut inventory = TextInventory {
        scripts: ScriptScan::default(),
        fonts: Vec::new(),
        coverage: Vec::new(),
    };
    let mut shape_count = 0u32;

    fn run_family(
        major: &Option<String>,
        minor: &Option<String>,
        category: Category,
        run: &TextRun,
    ) -> String {
        match run.typeface.as_deref() {
            Some("+mj-lt") => major.clone(),
            Some("+mn-lt") => minor.clone(),
            Some(family) if !family.starts_with('+') => Some(family.to_string()),
            _ if category == Category::Title => major.clone(),
            _ => minor.clone(),
        }
        .unwrap_or_else(|| "Calibri".to_string())
    }

    fn add_font(
        fonts: &mut Vec<String>,
        major: &Option<String>,
        minor: &Option<String>,
        category: Category,
        run: &TextRun,
    ) {
        let f = run_family(major, minor, category, run);
        if !fonts.contains(&f) {
            fonts.push(f);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_body(
        body: &TextBody,
        profile: &mut DocProfile,
        inventory: &mut TextInventory,
        major: &Option<String>,
        minor: &Option<String>,
        category: Category,
        doc: &PptxDoc,
        rels: &std::collections::HashMap<String, String>,
    ) {
        for detail in &body.unsupported {
            profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
        }
        for para in &body.paragraphs {
            for detail in &para.unsupported {
                let code = if detail.contains("fld") {
                    UnsupportedCode::FieldUnsupported
                } else {
                    UnsupportedCode::UnknownElement
                };
                profile.add_reason_deduped(code, detail.clone());
            }
            if let Some((format, _)) = &para.bullet_auto_num
                && format != "arabicPeriod"
            {
                profile.add_reason_deduped(
                    UnsupportedCode::UnknownElement,
                    format!("buAutoNum:{format}"),
                );
            }
            if !para.bullet_none
                && let Some(bullet) = &para.bullet_char
            {
                let mut mapped = String::new();
                for ch in bullet.chars() {
                    let ch = matching::map_symbol_char("Wingdings", ch as u32).unwrap_or(ch);
                    mapped.push(ch);
                }
                let run = para.runs.first();
                let fallback_run = TextRun::default();
                let family = run_family(major, minor, category, run.unwrap_or(&fallback_run));
                record_coverage(
                    &mut inventory.coverage,
                    &family,
                    if run.is_some_and(|run| run.bold == Some(true)) {
                        700
                    } else {
                        400
                    },
                    run.is_some_and(|run| run.italic == Some(true)),
                    &mapped,
                );
            }
            for run in &para.runs {
                for detail in &run.unsupported {
                    profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
                }
                inventory.scripts.merge(scan_scripts(&run.text));
                add_font(&mut inventory.fonts, major, minor, category, run);
                let family = run_family(major, minor, category, run);
                record_coverage(
                    &mut inventory.coverage,
                    &family,
                    if run.bold == Some(true) { 700 } else { 400 },
                    run.italic == Some(true),
                    &run.text,
                );
                if let Some(rel_id) = &run.hlink
                    && doc.link_target(rels, rel_id).is_none()
                {
                    profile.add_reason_deduped(
                        UnsupportedCode::Hyperlink,
                        format!("unsupported hyperlink relationship {rel_id}"),
                    );
                }
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "recursive PPTX profile walk carries shared inventory and relationship scope"
    )]
    fn walk(
        elems: &[ShapeElem],
        profile: &mut DocProfile,
        inventory: &mut TextInventory,
        major: &Option<String>,
        minor: &Option<String>,
        theme: &Theme,
        inheritance: Option<(&crate::oxml::pml::Slide, &crate::oxml::pml::Slide)>,
        shape_count: &mut u32,
        doc: &PptxDoc,
        rels: &std::collections::HashMap<String, String>,
        skip_placeholders: bool,
    ) {
        for elem in elems {
            match elem {
                // A group is not itself a drawn shape; recurse without counting it
                // so stats.shapes reflects leaf shapes only.
                ShapeElem::Group { children, .. } => walk(
                    children,
                    profile,
                    inventory,
                    major,
                    minor,
                    theme,
                    inheritance,
                    shape_count,
                    doc,
                    rels,
                    skip_placeholders,
                ),
                ShapeElem::Shape { ph, props, text } => {
                    if skip_placeholders && ph.is_some() {
                        continue;
                    }
                    *shape_count += 1;
                    for detail in &props.unsupported {
                        profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
                    }
                    if let Some(preset) = &props.preset
                        && !supports_preset(&preset.name)
                    {
                        profile.add_reason_deduped(
                            UnsupportedCode::ShapeGeometry,
                            format!("preset:{}", preset.name),
                        );
                    }
                    let xfrm = props.xfrm.unwrap_or(crate::oxml::pml::Xfrm {
                        w_pt: 100.0,
                        h_pt: 100.0,
                        present: true,
                        ..Default::default()
                    });
                    if let Err(error) = evaluate_shape(props, xfrm) {
                        profile
                            .add_reason_deduped(UnsupportedCode::ShapeGeometry, error.to_string());
                    }
                    if let Some(warp) = &props.text_warp
                        && warp != "textNoShape"
                        && warp != "none"
                    {
                        profile.add_reason_deduped(
                            UnsupportedCode::WordArt,
                            format!("a:prstTxWarp:{warp}"),
                        );
                    }
                    if let Some(fill) = &props.fill {
                        profile_pptx_fill(fill, theme, profile, doc, rels);
                    }
                    if let Some(body) = text {
                        let resolved_body = inheritance.map_or_else(
                            || body.clone(),
                            |(layout, master)| resolve_text_body(ph.as_ref(), body, layout, master),
                        );
                        let category = ph
                            .as_ref()
                            .map(crate::pptx::inherit::category)
                            .unwrap_or(Category::Other);
                        walk_body(
                            &resolved_body,
                            profile,
                            inventory,
                            major,
                            minor,
                            category,
                            doc,
                            rels,
                        );
                    }
                }
                ShapeElem::Picture { props, drawing } => {
                    *shape_count += 1;
                    if let Some(error) = &drawing.unsupported {
                        profile.add_reason_deduped(error.code, error.detail.clone());
                    } else if let Some(rel_id) = &drawing.blip_rel_id {
                        if let Some((code, detail)) = doc.image_error(rels, rel_id, drawing.crop) {
                            profile.add_reason_deduped(code, detail);
                        }
                    } else {
                        profile.add_reason_deduped(
                            UnsupportedCode::MissingRelationshipTarget,
                            "p:pic missing a:blip",
                        );
                    }
                    if let Some(preset) = &props.preset
                        && preset.name != "rect"
                    {
                        profile.add_reason_deduped(
                            UnsupportedCode::ShapeGeometry,
                            format!("picture frame preset:{}", preset.name),
                        );
                    }
                }
                ShapeElem::Table { table, .. } => {
                    *shape_count += 1;
                    if let Some(fill) = &table.fill {
                        profile_pptx_fill(fill, theme, profile, doc, rels);
                    }
                    for (row_index, row) in table.rows.iter().enumerate() {
                        let mut col_index = 0usize;
                        for cell in &row.cells {
                            match doc.table_cell_style(table, row_index, col_index) {
                                Ok(Some(style)) => {
                                    if let Some(fill) = &style.fill {
                                        profile_pptx_fill(fill, theme, profile, doc, rels);
                                    }
                                }
                                Ok(None) => {}
                                Err(OfficeError::Unsupported { code, detail }) => {
                                    profile.add_reason_deduped(code, detail)
                                }
                                Err(error) => profile.add_reason_deduped(
                                    UnsupportedCode::TableStyle,
                                    error.to_string(),
                                ),
                            }
                            for detail in &cell.unsupported {
                                profile.add_reason_deduped(
                                    UnsupportedCode::UnknownElement,
                                    detail.clone(),
                                );
                            }
                            if let Some(fill) = &cell.fill {
                                profile_pptx_fill(fill, theme, profile, doc, rels);
                            }
                            if let Some(b) = &cell.text {
                                walk_body(
                                    b,
                                    profile,
                                    inventory,
                                    major,
                                    minor,
                                    Category::Other,
                                    doc,
                                    rels,
                                );
                            }
                            col_index += cell.grid_span.max(1) as usize;
                        }
                    }
                }
                ShapeElem::Chart { xfrm, rel_id } => {
                    *shape_count += 1;
                    if let Some((code, detail)) = doc.chart_error(rels, rel_id) {
                        profile.add_reason_deduped(code, detail);
                    } else if let Some(chart) = doc.chart(rels, rel_id) {
                        let frame = xfrm.unwrap_or(crate::oxml::pml::Xfrm {
                            w_pt: 360.0,
                            h_pt: 216.0,
                            present: true,
                            ..Default::default()
                        });
                        if let Err(error) = chart.render(&doc.fonts, theme, frame.w_pt, frame.h_pt)
                        {
                            match error {
                                OfficeError::Unsupported { code, detail } => {
                                    profile.add_reason_deduped(code, detail)
                                }
                                OfficeError::Font(detail) => profile
                                    .add_reason_deduped(UnsupportedCode::MissingGlyph, detail),
                                other => profile.add_reason_deduped(
                                    UnsupportedCode::Chart,
                                    format!("chart render validation: {other}"),
                                ),
                            }
                        }
                        if !inventory.fonts.iter().any(|font| font == "Calibri") {
                            inventory.fonts.push("Calibri".to_string());
                        }
                    }
                }
                ShapeElem::SmartArt { data_rel_id, .. } => {
                    if let Some((code, detail)) = doc.diagram_error(rels, data_rel_id) {
                        *shape_count += 1;
                        profile.add_reason_deduped(code, detail);
                    } else if let Some(diagram) = doc.diagram(rels, data_rel_id) {
                        walk(
                            &diagram.shapes,
                            profile,
                            inventory,
                            major,
                            minor,
                            theme,
                            None,
                            shape_count,
                            doc,
                            &diagram.rels,
                            false,
                        );
                    }
                }
                ShapeElem::OlePreview { preview_rel_id, .. } => {
                    *shape_count += 1;
                    if let Some((code, detail)) = doc.ole_preview_error(rels, preview_rel_id) {
                        profile.add_reason_deduped(code, detail);
                    }
                }
                ShapeElem::Unsupported(detail) => {
                    *shape_count += 1;
                    let code = if detail == "chart" {
                        UnsupportedCode::Chart
                    } else if detail.starts_with("smartart") {
                        UnsupportedCode::SmartArt
                    } else if detail.starts_with("ole") {
                        UnsupportedCode::OleObject
                    } else {
                        UnsupportedCode::UnknownElement
                    };
                    profile.add_reason_deduped(code, detail.clone());
                }
            }
        }
    }

    for rs in &doc.slides {
        let major = rs.theme.major_latin.clone();
        let minor = rs.theme.minor_latin.clone();
        if let Some(fill) = rs.slide.background.as_ref() {
            profile_pptx_fill(fill, &rs.theme, &mut profile, &doc, &rs.rels);
        } else if let Some(fill) = rs.layout.background.as_ref() {
            profile_pptx_fill(fill, &rs.theme, &mut profile, &doc, &rs.layout_rels);
        } else if let Some(fill) = rs.master.background.as_ref() {
            profile_pptx_fill(fill, &rs.theme, &mut profile, &doc, &rs.master_rels);
        }
        if rs.slide.show_master_shapes != Some(false) {
            walk(
                &rs.master.shapes,
                &mut profile,
                &mut inventory,
                &major,
                &minor,
                &rs.theme,
                None,
                &mut shape_count,
                &doc,
                &rs.master_rels,
                true,
            );
        }
        walk(
            &rs.layout.shapes,
            &mut profile,
            &mut inventory,
            &major,
            &minor,
            &rs.theme,
            None,
            &mut shape_count,
            &doc,
            &rs.layout_rels,
            true,
        );
        walk(
            &rs.slide.shapes,
            &mut profile,
            &mut inventory,
            &major,
            &minor,
            &rs.theme,
            Some((&rs.layout, &rs.master)),
            &mut shape_count,
            &doc,
            &rs.rels,
            false,
        );
    }

    resolve_fonts(
        &mut profile,
        &inventory.coverage,
        &doc.fonts,
        &inventory.scripts,
    );
    profile.fonts = font_resolution_report(&inventory.coverage, &doc.fonts);
    add_fallback_face_missing_reason(&mut profile, &doc.fonts, &inventory.coverage);
    profile.font_requests = canonical_font_requests(&inventory.coverage, None, &doc.fonts);
    profile.stats.slides = doc.slides.len() as u32;
    profile.stats.shapes = shape_count;
    profile.stats.images = doc.images.len() as u32;
    profile.stats.fonts_used = inventory.fonts;
    Ok(profile)
}

fn profile_pptx_fill(
    fill: &crate::oxml::pml::Fill,
    theme: &Theme,
    profile: &mut DocProfile,
    doc: &crate::pptx::PptxDoc,
    rels: &HashMap<String, String>,
) {
    if let crate::oxml::pml::Fill::Unsupported(error) = fill {
        profile.add_reason_deduped(error.code, error.detail.clone());
    } else if let Some(slot) = fill.unresolved_scheme(theme) {
        profile.add_reason_deduped(
            UnsupportedCode::InvalidFill,
            format!("fill references unresolved theme color {slot}"),
        );
    }
    if let crate::oxml::pml::Fill::Picture(picture) = fill
        && let Some((code, detail)) = doc.image_error(rels, &picture.rel_id, picture.crop)
    {
        profile.add_reason_deduped(code, detail);
    }
}

fn profile_docx(bytes: &[u8], opts: &crate::ConvertOptions) -> Result<DocProfile> {
    // Container-level failures propagate as Err (allowed by the contract).
    let pkg = Package::open(bytes)?;
    let main = pkg.main_part()?;
    let mut document = parse_part(&pkg, &main, wml::parse_document)?;

    let styles = optional_part(&pkg, "word/styles.xml", StyleResolver::parse)?.unwrap_or_default();
    // The convert path resolves table styles before layout; profile must see the
    // same resolved tree, or a style-supplied property would be reported as
    // unsupported by one and not the other.
    table_style::apply_table_styles(&styles, &mut document.body);
    let numbering =
        optional_part(&pkg, "word/numbering.xml", Numbering::parse)?.unwrap_or_default();
    let settings = optional_part(&pkg, "word/settings.xml", Settings::parse)?.unwrap_or_default();
    let theme = find_theme(&pkg)?;
    let font_table = optional_part(&pkg, "word/fontTable.xml", FontTable::parse)?;
    let diagrams = DiagramStore::load(&pkg)?;

    let mut db = FontDb::with_sources(&opts.font_dirs, opts.include_system_fonts);
    db.set_resolution_mode(opts.resolution_mode);
    for bytes in &opts.font_bytes {
        db.register(bytes.clone())?;
    }
    crate::register_runtime_fonts(&mut db)?;
    db.mark_hash_pinned(&opts.font_hash_pins);
    crate::layout::assemble::register_embedded_fonts(&pkg, &mut db);
    if let Some(font_table) = &font_table {
        font_table.install_classing_hints(&mut db);
    }

    let mut profile = DocProfile::new("docx");
    let mut collector = Collector::new(&styles, &numbering, &theme, &diagrams, &db);

    // Body content. `word/comments.xml` is intentionally excluded: comments do
    // not paint in Word's print view, so their text and fonts are not render inputs.
    let main_rels: HashMap<_, _> = pkg
        .relationships(&main)?
        .into_iter()
        .map(|relationship| (relationship.id, relationship.target))
        .collect();
    collector.walk_blocks(
        &document.body,
        &pkg,
        &main_rels,
        &mut profile,
        StoryContext::Body,
    );

    // Headers/footers. A parse failure here is not swallowed: an unsupported or
    // malformed header must surface (as it would during layout), not silently
    // pass the profile gate.
    let mut profiled_parts = Vec::new();
    for reference in header_footer_refs(&document) {
        let Some(part) = main_rels.get(&reference.rel_id) else {
            profile.add_reason_deduped(
                UnsupportedCode::MissingRelationshipTarget,
                format!("header/footer relationship {} is missing", reference.rel_id),
            );
            continue;
        };
        if profiled_parts.contains(part) {
            continue;
        }
        let Some(bytes) = pkg.part(part) else {
            profile.add_reason_deduped(
                UnsupportedCode::MissingRelationshipTarget,
                format!("header/footer part {part} is missing"),
            );
            continue;
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|e| OfficeError::Invalid(format!("header/footer {part}: {e}")))?;
        let (mut blocks, flags) = wml::parse_hdr_ftr_with_flags(text)
            .map_err(|error| error.context(format_args!("header/footer {part}")))?;
        table_style::apply_table_styles(&styles, &mut blocks);
        add_flag_reasons(&mut profile, &flags);
        let hf_rels = rel_map(&pkg, part);
        collector.walk_blocks(
            &blocks,
            &pkg,
            &hf_rels,
            &mut profile,
            StoryContext::HeaderFooter,
        );
        profiled_parts.push(part.clone());
    }

    let mut available_footnotes = Vec::new();
    let mut available_endnotes = Vec::new();
    for (part, kind) in [
        ("word/footnotes.xml", wml::NoteKind::Footnote),
        ("word/endnotes.xml", wml::NoteKind::Endnote),
    ] {
        if pkg.part(part).is_some() {
            let mut notes = parse_part(&pkg, part, |xml| wml::parse_notes(xml, kind))?;
            table_style::apply_notes_table_styles(&styles, &mut notes);
            match kind {
                wml::NoteKind::Footnote => available_footnotes.extend(notes.notes.keys().copied()),
                wml::NoteKind::Endnote => available_endnotes.extend(notes.notes.keys().copied()),
            }
            add_flag_reasons(&mut profile, &notes.flags);
            let note_rels = rel_map(&pkg, part);
            for blocks in notes.notes.values() {
                collector.walk_blocks(blocks, &pkg, &note_rels, &mut profile, StoryContext::Note);
            }
        }
    }
    for &(kind, id) in &collector.note_refs {
        let (available, code, label) = match kind {
            wml::NoteKind::Footnote => {
                (&available_footnotes, UnsupportedCode::Footnotes, "footnote")
            }
            wml::NoteKind::Endnote => (&available_endnotes, UnsupportedCode::Endnotes, "endnote"),
        };
        if !available.contains(&id) {
            profile.add_reason_deduped(code, format!("missing {label} {id}"));
        }
    }

    // Reasons from parse-time document flags + settings.
    add_flag_reasons(&mut profile, &document.flags);
    add_section_reasons(&mut profile, &document);
    add_anchor_column_reasons(&mut profile, &document);
    if settings.auto_hyphenation {
        profile.add_reason_deduped(UnsupportedCode::AutoHyphenation, "w:autoHyphenation");
    }
    for detail in &settings.visible_unsupported {
        if detail.starts_with("m:mathPr") && !collector.has_math {
            continue;
        }
        profile.add_reason_deduped(
            if detail.starts_with("m:mathPr") {
                UnsupportedCode::Equation
            } else {
                UnsupportedCode::UnknownElement
            },
            detail.clone(),
        );
    }

    // Font resolution: warnings + missing fonts (deterministic, bundled DB only).
    // Sets `profile.stats.fonts_missing` directly.
    resolve_fonts(&mut profile, &collector.coverage, &db, &collector.scripts);
    profile.fonts = font_resolution_report(&collector.coverage, &db);
    add_fallback_face_missing_reason(&mut profile, &db, &collector.coverage);

    profile.stats.paragraphs = collector.paragraphs;
    profile.stats.tables = collector.tables;
    profile.stats.images = collector.images;
    profile.stats.fonts_used = collector.fonts_used.clone();
    profile.font_requests = canonical_font_requests(&collector.coverage, font_table.as_ref(), &db);

    // Some pagination verdicts depend on shaped line heights and physical-page
    // state. Run the same layout preflight used by conversion for those cases
    // so profile() cannot approve a document that layout rejects.
    let body_anchor_count: usize = document
        .body
        .iter()
        .filter_map(|block| match block {
            BlockContent::Paragraph(paragraph) => Some(paragraph_anchor_count(paragraph)),
            BlockContent::Table(_) => None,
        })
        .sum();
    let body_has_page_float = document.body.iter().any(|block| match block {
        BlockContent::Paragraph(paragraph) => paragraph_has_page_float(paragraph),
        BlockContent::Table(_) => false,
    });
    let body_has_floating_table = blocks_have_floating_table(&document.body);
    let has_footnote_references = collector
        .note_refs
        .iter()
        .any(|(kind, _)| *kind == wml::NoteKind::Footnote);
    let mut sections: Vec<&SectPr> = document
        .body
        .iter()
        .filter_map(|block| match block {
            BlockContent::Paragraph(paragraph) => paragraph.sect_break.as_ref(),
            BlockContent::Table(_) => None,
        })
        .collect();
    sections.push(&document.final_sect);
    let has_multi_section_columns =
        sections.len() >= 2 && sections.iter().any(|section| section.cols.is_multi());
    if profile.supported
        && (body_anchor_count > 1
            || body_has_page_float
            || body_has_floating_table
            || has_footnote_references
            || collector.has_tab_leaders
            || has_multi_section_columns)
    {
        let layout_doc = crate::layout::WordDoc::open(bytes, opts)?;
        match crate::layout::layout_docx(&layout_doc) {
            Ok((_, warnings)) => {
                for warning in warnings {
                    profile.add_warning_deduped(warning);
                }
            }
            Err(error) => match error {
                OfficeError::Unsupported { code, detail } => {
                    profile.add_reason_deduped(code, detail)
                }
                OfficeError::Font(detail) => {
                    profile.add_reason_deduped(UnsupportedCode::MissingGlyph, detail)
                }
                OfficeError::Invalid(detail) | OfficeError::Internal(detail) => {
                    profile.add_reason_deduped(UnsupportedCode::InvalidFile, detail)
                }
            },
        }
    }

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
        (flags.image_crop, UnsupportedCode::ImageCrop, "a:srcRect"),
        (
            flags.vml_pict,
            UnsupportedCode::UnknownElement,
            "w:pict (VML)",
        ),
        (flags.ole_object, UnsupportedCode::OleObject, "w:object"),
        (
            flags.wordart_warp,
            UnsupportedCode::WordArt,
            "unsupported:wordart-warp",
        ),
    ];
    for &(present, code, detail) in mapping {
        if present {
            profile.add_reason_deduped(code, detail);
        }
    }
    for detail in &flags.visible_unsupported {
        profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
    }
}

fn add_section_reasons(profile: &mut DocProfile, document: &wml::Document) {
    let mut sections: Vec<&SectPr> = document
        .body
        .iter()
        .filter_map(|block| match block {
            BlockContent::Paragraph(paragraph) => paragraph.sect_break.as_ref(),
            BlockContent::Table(_) => None,
        })
        .collect();
    sections.push(&document.final_sect);
    for section in &sections {
        if let NumFmt::Other(value) = NumFmt::parse(&section.page_num_fmt) {
            profile.add_reason_deduped(
                UnsupportedCode::UnknownElement,
                format!("pgNumType@fmt:{value}"),
            );
        }
    }
    for pair in sections.windows(2) {
        let previous = pair[0];
        let current = pair[1];
        if current.sect_type == SectType::Continuous && section_geometry_differs(previous, current)
        {
            profile.add_reason_deduped(
                UnsupportedCode::UnknownElement,
                "continuous section break changes page geometry",
            );
        }
    }
}

fn add_anchor_column_reasons(profile: &mut DocProfile, document: &wml::Document) {
    let boundaries: Vec<(usize, &SectPr)> = document
        .body
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            BlockContent::Paragraph(paragraph) => paragraph
                .sect_break
                .as_ref()
                .map(|section| (index, section)),
            BlockContent::Table(_) => None,
        })
        .collect();
    let mut start = 0usize;
    for (block_index, section) in &boundaries {
        if section.cols.is_multi() && blocks_have_body_anchor(&document.body[start..=*block_index])
        {
            profile.add_reason_deduped(
                UnsupportedCode::AnchoredDrawing,
                "anchored drawing in a multi-column section",
            );
        }
        start = *block_index + 1;
    }
    if document.final_sect.cols.is_multi() && blocks_have_body_anchor(&document.body[start..]) {
        profile.add_reason_deduped(
            UnsupportedCode::AnchoredDrawing,
            "anchored drawing in a multi-column section",
        );
    }
}

fn blocks_have_body_anchor(blocks: &[BlockContent]) -> bool {
    blocks.iter().any(|block| match block {
        BlockContent::Paragraph(paragraph) => paragraph_anchor_count(paragraph) > 0,
        BlockContent::Table(_) => false,
    })
}

fn blocks_have_floating_table(blocks: &[BlockContent]) -> bool {
    blocks.iter().any(|block| match block {
        BlockContent::Paragraph(_) => false,
        BlockContent::Table(table) => {
            table.props.floating.is_some()
                || table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .any(|cell| blocks_have_floating_table(&cell.content))
        }
    })
}

fn section_geometry_differs(left: &SectPr, right: &SectPr) -> bool {
    left.page_w_pt != right.page_w_pt
        || left.page_h_pt != right.page_h_pt
        || left.orientation != right.orientation
        || left.margins.top != right.margins.top
        || left.margins.right != right.margins.right
        || left.margins.bottom != right.margins.bottom
        || left.margins.left != right.margins.left
        || left.margins.gutter != right.margins.gutter
}

/// Resolves each requested font family, accumulating warnings + missing fonts,
/// and flags CJK text that no available face covers.
fn resolve_fonts(
    profile: &mut DocProfile,
    requests: &[CoverageRequest],
    db: &FontDb,
    scripts: &ScriptScan,
) {
    let mut missing = Vec::new();
    let mut unresolved = Vec::new();
    for request in requests {
        let resolved = matching::resolve_with_stretch(
            db,
            &request.family,
            request.weight,
            request.italic,
            request.stretch,
        );
        for w in resolved.warnings {
            profile.add_warning_deduped(w);
        }
        if let Some(m) = resolved.missing
            && !missing
                .iter()
                .any(|family: &String| family.eq_ignore_ascii_case(&m))
        {
            missing.push(m);
        }
        if resolved.face.is_none() {
            let identity = format!(
                "{} weight={} italic={} stretch={}",
                request.family, request.weight, request.italic, request.stretch
            );
            if !unresolved.contains(&identity) {
                unresolved.push(identity);
            }
        }
    }
    // CJK with no covering face is fail-closed.
    if scripts.cjk && !db_covers_cjk(db) {
        profile.add_reason_deduped(UnsupportedCode::UnknownElement, "no font for CJK");
    }
    // Store missing on the collector via the profile stats (set by caller).
    profile.stats.fonts_missing = missing;
    if !unresolved.is_empty() {
        profile.add_reason_deduped(
            UnsupportedCode::FontUnresolved,
            format!("unresolved font faces: {}", unresolved.join(", ")),
        );
    }
}

/// Whether any face in `db` maps a representative CJK ideograph.
fn db_covers_cjk(db: &FontDb) -> bool {
    db.faces().iter().any(|face| {
        face.parsed()
            .and_then(|f| f.glyph_index('\u{65E5}'))
            .is_some()
    })
}

fn collect_codepoints(codepoints: &mut BTreeSet<char>, text: &str) {
    codepoints.extend(text.chars().filter(|ch| !ch.is_control()));
}

struct CoverageRequest {
    family: String,
    weight: u16,
    italic: bool,
    stretch: u16,
    slots: BTreeSet<FontSlot>,
    codepoints: BTreeSet<char>,
    texts: Vec<String>,
}

fn record_coverage(
    requests: &mut Vec<CoverageRequest>,
    family: &str,
    weight: u16,
    italic: bool,
    text: &str,
) {
    let slots = inferred_slots(text);
    record_coverage_with_slots(requests, family, weight, italic, 5, text, &slots);
}

fn record_coverage_with_slots(
    requests: &mut Vec<CoverageRequest>,
    family: &str,
    weight: u16,
    italic: bool,
    stretch: u16,
    text: &str,
    slots: &BTreeSet<FontSlot>,
) {
    let request = requests.iter_mut().find(|request| {
        request.family.eq_ignore_ascii_case(family)
            && request.weight == weight
            && request.italic == italic
            && request.stretch == stretch
    });
    if let Some(request) = request {
        collect_codepoints(&mut request.codepoints, text);
        request.slots.extend(slots.iter().copied());
        if !request.texts.iter().any(|candidate| candidate == text) {
            request.texts.push(text.to_string());
        }
    } else {
        let mut codepoints = BTreeSet::new();
        collect_codepoints(&mut codepoints, text);
        requests.push(CoverageRequest {
            family: family.to_string(),
            weight,
            italic,
            stretch,
            slots: slots.clone(),
            codepoints,
            texts: vec![text.to_string()],
        });
    }
}

fn inferred_slots(text: &str) -> BTreeSet<FontSlot> {
    text.chars()
        .filter(|character| !character.is_control())
        .map(slot_for_character)
        .collect()
}

fn slot_for_character(character: char) -> FontSlot {
    let codepoint = character as u32;
    if character.is_ascii() {
        FontSlot::Ascii
    } else if matches!(
        codepoint,
        0x4E00..=0x9FFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF | 0x3000..=0x303F
    ) {
        FontSlot::EastAsia
    } else if matches!(
        codepoint,
        0x0590..=0x08FF
            | 0x0900..=0x0EFF
            | 0x1000..=0x109F
            | 0x1780..=0x17FF
            | 0xFB1D..=0xFEFF
    ) {
        FontSlot::Cs
    } else {
        FontSlot::HAnsi
    }
}

fn numbering_request_text(pattern: &str) -> String {
    let mut output = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '%' && characters.peek().is_some_and(char::is_ascii_digit) {
            characters.next();
            output.push('0');
        } else {
            output.push(character);
        }
    }
    output
}

fn canonical_font_requests(
    coverage: &[CoverageRequest],
    font_table: Option<&FontTable>,
    db: &FontDb,
) -> Vec<FontRequest> {
    let mut requests = Vec::new();
    for coverage in coverage {
        if matching::resolve_exact_with_stretch(
            db,
            &coverage.family,
            coverage.weight,
            coverage.italic,
            coverage.stretch,
        )
        .is_some()
        {
            continue;
        }
        let metadata = font_table.and_then(|table| table.find(&coverage.family));
        if metadata.is_some_and(|entry| entry.has_embedded_style(coverage.weight, coverage.italic))
        {
            continue;
        }
        let mut slots: Vec<_> = coverage.slots.iter().copied().collect();
        slots.sort();
        let mut scripts = scripts_for_codepoints(&coverage.codepoints);
        scripts.sort();
        let mut alt_families = metadata
            .and_then(|entry| entry.alt_name.clone())
            .into_iter()
            .filter(|name| !name.eq_ignore_ascii_case(&coverage.family))
            .collect::<Vec<_>>();
        alt_families.sort_by_key(|name| name.to_ascii_lowercase());
        requests.push(FontRequest {
            family: coverage.family.clone(),
            alt_families,
            weight: coverage.weight,
            italic: coverage.italic,
            stretch: coverage.stretch,
            slots,
            panose: metadata.and_then(|entry| entry.panose.clone()),
            family_class: metadata.and_then(|entry| entry.family_class.clone()),
            pitch: metadata.and_then(|entry| entry.pitch.clone()),
            charset: metadata.and_then(|entry| entry.charset.clone()),
            unicode_ranges: metadata
                .map(|entry| entry.unicode_ranges.clone())
                .unwrap_or_default(),
            scripts,
            vf_coords: Vec::new(),
        });
    }
    requests.sort_by(|left, right| {
        left.family
            .to_ascii_lowercase()
            .cmp(&right.family.to_ascii_lowercase())
            .then_with(|| left.weight.cmp(&right.weight))
            .then_with(|| left.italic.cmp(&right.italic))
            .then_with(|| left.stretch.cmp(&right.stretch))
    });
    requests
}

fn scripts_for_codepoints(codepoints: &BTreeSet<char>) -> Vec<String> {
    let mut scripts = BTreeSet::new();
    for character in codepoints {
        let codepoint = *character as u32;
        let script = if character.is_ascii_alphabetic() {
            "latin"
        } else if character.is_ascii() {
            "common"
        } else if matches!(codepoint, 0x00C0..=0x024F | 0x1E00..=0x1EFF) {
            "latin"
        } else if matches!(codepoint, 0x0370..=0x03FF) {
            "greek"
        } else if matches!(codepoint, 0x0400..=0x052F) {
            "cyrillic"
        } else if matches!(codepoint, 0x0590..=0x05FF) {
            "hebrew"
        } else if matches!(codepoint, 0x0600..=0x08FF | 0xFB50..=0xFEFF) {
            "arabic"
        } else if matches!(codepoint, 0x0900..=0x097F) {
            "devanagari"
        } else if matches!(codepoint, 0x0E00..=0x0E7F) {
            "thai"
        } else if matches!(
            codepoint,
            0x3000..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
        ) {
            "cjk"
        } else if !character.is_alphanumeric() && !character.is_whitespace() {
            "symbols"
        } else {
            "common"
        };
        scripts.insert(script.to_string());
    }
    scripts.into_iter().collect()
}

fn font_resolution_report(requests: &[CoverageRequest], db: &FontDb) -> Vec<FontResolution> {
    let mut report = Vec::new();
    for request in requests {
        let resolved = matching::resolve_with_stretch(
            db,
            &request.family,
            request.weight,
            request.italic,
            request.stretch,
        );
        let mut slots: Vec<_> = request.slots.iter().copied().collect();
        slots.sort();
        let Some(face_id) = resolved.face else {
            report.push(FontResolution {
                requested_family: request.family.clone(),
                requested_weight: request.weight,
                requested_italic: request.italic,
                requested_stretch: request.stretch,
                slots,
                selected: None,
                source: FontResolutionSource::None,
                exactness: FontExactness::Unresolved,
                embedding_permission: None,
                coverage_ok: false,
                failure_reason: Some("font_unresolved".to_string()),
                fallback_identity: None,
                cache_status: None,
                fetch_duration_ms: None,
                event: FontResolutionEvent::FontNotFound,
            });
            continue;
        };
        let face = db.face(face_id).expect("resolved face exists");
        let selected = selected_font_identity(face);
        let exact_face = matching::resolve_exact_with_stretch(
            db,
            &request.family,
            request.weight,
            request.italic,
            request.stretch,
        ) == Some(face_id);
        let same_family = face.family_name.eq_ignore_ascii_case(&request.family)
            || face
                .full_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(&request.family));
        let exactness = if face.embedded && exact_face {
            FontExactness::EmbeddedExact
        } else if exact_face && face.hash_pinned {
            FontExactness::HashPinnedExact
        } else if exact_face {
            FontExactness::CatalogVersionAssumed
        } else if same_family {
            FontExactness::FamilyStyleCompatible
        } else {
            FontExactness::Fallback
        };
        let source = match exactness {
            FontExactness::EmbeddedExact => FontResolutionSource::Embedded,
            FontExactness::Fallback => FontResolutionSource::Fallback,
            _ => FontResolutionSource::Registered,
        };
        let event = match exactness {
            FontExactness::EmbeddedExact => FontResolutionEvent::FontEmbedded,
            FontExactness::Fallback => FontResolutionEvent::FontFallbackUsed,
            FontExactness::CatalogVersionAssumed => FontResolutionEvent::FontVersionAssumed,
            _ => FontResolutionEvent::FontRuntimeRegistered,
        };
        let fallback_ids =
            matching::coverage_fallbacks(db, face_id, request.weight, request.italic);
        let coverage_ok = request.codepoints.iter().all(|character| {
            fallback_ids.iter().any(|candidate| {
                db.face(*candidate)
                    .and_then(|candidate| candidate.parsed())
                    .and_then(|candidate| candidate.glyph_index(*character))
                    .is_some()
            })
        });
        report.push(FontResolution {
            requested_family: request.family.clone(),
            requested_weight: request.weight,
            requested_italic: request.italic,
            requested_stretch: request.stretch,
            slots,
            selected: Some(selected.clone()),
            source,
            exactness,
            embedding_permission: Some(face.embedding_permission),
            coverage_ok,
            failure_reason: (!coverage_ok).then(|| "font_coverage_mismatch".to_string()),
            fallback_identity: (exactness == FontExactness::Fallback).then_some(selected),
            cache_status: None,
            fetch_duration_ms: None,
            event,
        });
    }
    report.sort_by(|left, right| {
        left.requested_family
            .to_ascii_lowercase()
            .cmp(&right.requested_family.to_ascii_lowercase())
            .then_with(|| left.requested_weight.cmp(&right.requested_weight))
            .then_with(|| left.requested_italic.cmp(&right.requested_italic))
    });
    report
}

fn selected_font_identity(face: &crate::fonts::Face) -> SelectedFontIdentity {
    let parsed = face.parsed();
    SelectedFontIdentity {
        family: face.family_name.clone(),
        full_name: face.full_name.clone(),
        postscript_name: parsed
            .as_ref()
            .and_then(|parsed| profile_face_name(parsed, ttf_parser::name_id::POST_SCRIPT_NAME)),
        version: parsed
            .as_ref()
            .and_then(|parsed| profile_face_name(parsed, ttf_parser::name_id::VERSION)),
        sha256: face.sha256.clone(),
        face_index: face.index,
        weight: face.weight,
        italic: face.italic,
        stretch: face.stretch,
    }
}

fn profile_face_name(face: &ttf_parser::Face<'_>, id: u16) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|name| name.name_id == id)
        .find_map(|name| name.to_string())
        .filter(|name| !name.is_empty())
}

fn add_fallback_face_missing_reason(
    profile: &mut DocProfile,
    db: &FontDb,
    requests: &[CoverageRequest],
) {
    let mut missing = BTreeSet::new();
    for request in requests {
        let Some(primary) =
            matching::resolve(db, &request.family, request.weight, request.italic).face
        else {
            continue;
        };
        let ids = matching::coverage_fallbacks(db, primary, request.weight, request.italic);
        let faces: Vec<_> = ids
            .iter()
            .filter_map(|id| db.face(*id))
            .filter_map(|face| text_core::Face::new(&face.data, face.index).ok())
            .collect();
        for text in &request.texts {
            match text_core::shape_text_with_features(
                text,
                &faces,
                text_core::FeatureConfig::default().kern(false).liga(false),
            ) {
                Ok(_) => {}
                Err(text_core::ShapeError::MissingGlyphs(chars)) => missing.extend(chars),
                Err(_) => missing.extend(text.chars().filter(|ch| !ch.is_control())),
            }
        }
    }
    let detail = missing
        .iter()
        .take(4)
        .map(|ch| format!("U+{:04X}", *ch as u32))
        .collect::<Vec<_>>()
        .join(", ");
    if !detail.is_empty() {
        profile.add_reason_deduped(
            UnsupportedCode::MissingGlyph,
            format!("no registered font covers {detail}"),
        );
    }
}

fn paragraph_anchor_count(paragraph: &Paragraph) -> usize {
    fn run_count(run: &wml::Run) -> usize {
        run.content
            .iter()
            .filter(|content| {
                matches!(content, RunContent::Drawing(drawing) if drawing.anchor.is_some())
                    || matches!(content, RunContent::VmlShape(_))
            })
            .count()
    }

    fn inline_count(inline: &InlineContent) -> usize {
        match inline {
            InlineContent::Bookmark(_) => 0,
            InlineContent::Run(run) => run_count(run),
            InlineContent::Hyperlink { runs, .. } => runs.iter().map(run_count).sum(),
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                field.content.iter().map(inline_count).sum()
            }
            InlineContent::Math(_) => 0,
        }
    }

    paragraph.runs.iter().map(inline_count).sum()
}

fn paragraph_has_tab(paragraph: &Paragraph) -> bool {
    fn run_has_tab(run: &wml::Run) -> bool {
        run.content
            .iter()
            .any(|content| matches!(content, RunContent::Tab))
    }

    fn inline_has_tab(inline: &InlineContent) -> bool {
        match inline {
            InlineContent::Bookmark(_) | InlineContent::Math(_) => false,
            InlineContent::Run(run) => run_has_tab(run),
            InlineContent::Hyperlink { runs, .. } => runs.iter().any(run_has_tab),
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                field.content.iter().any(inline_has_tab)
            }
        }
    }

    paragraph.runs.iter().any(inline_has_tab)
}

fn paragraph_has_page_float(paragraph: &Paragraph) -> bool {
    fn run_has_page_float(run: &wml::Run) -> bool {
        run.content.iter().any(|content| {
            matches!(
                content,
                RunContent::Drawing(drawing)
                    if drawing.anchor.is_some_and(|anchor| {
                        anchor.relative_from_v != wml::AnchorOrigin::Paragraph
                    })
            )
        })
    }

    fn inline_has_page_float(inline: &InlineContent) -> bool {
        match inline {
            InlineContent::Bookmark(_) => false,
            InlineContent::Run(run) => run_has_page_float(run),
            InlineContent::Hyperlink { runs, .. } => runs.iter().any(run_has_page_float),
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                field.content.iter().any(inline_has_page_float)
            }
            InlineContent::Math(_) => false,
        }
    }

    paragraph.runs.iter().any(inline_has_page_float)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoryContext {
    Body,
    HeaderFooter,
    TableCell,
    TextBox,
    Note,
}

impl StoryContext {
    fn anchor_detail(self) -> Option<&'static str> {
        match self {
            StoryContext::Body => None,
            StoryContext::HeaderFooter => Some("anchored drawing in a header or footer"),
            StoryContext::TableCell => Some("anchored drawing in a table cell"),
            StoryContext::TextBox => Some("anchored drawing in a text box"),
            StoryContext::Note => Some("anchored drawing in a footnote or endnote"),
        }
    }
}

/// A walker accumulating stats, fonts, scripts, and content-derived reasons.
struct Collector<'a> {
    styles: &'a StyleResolver,
    numbering: &'a Numbering,
    theme: &'a Theme,
    diagrams: &'a DiagramStore,
    font_db: &'a FontDb,
    fonts_used: Vec<String>,
    scripts: ScriptScan,
    coverage: Vec<CoverageRequest>,
    paragraphs: u32,
    tables: u32,
    images: u32,
    validated_images: BTreeSet<String>,
    total_image_pixels: u64,
    has_math: bool,
    has_tab_leaders: bool,
    note_refs: Vec<(wml::NoteKind, i64)>,
}

impl<'a> Collector<'a> {
    fn new(
        styles: &'a StyleResolver,
        numbering: &'a Numbering,
        theme: &'a Theme,
        diagrams: &'a DiagramStore,
        font_db: &'a FontDb,
    ) -> Self {
        Collector {
            styles,
            numbering,
            theme,
            diagrams,
            font_db,
            fonts_used: Vec::new(),
            scripts: ScriptScan::default(),
            coverage: Vec::new(),
            paragraphs: 0,
            tables: 0,
            images: 0,
            validated_images: BTreeSet::new(),
            total_image_pixels: 0,
            has_math: false,
            has_tab_leaders: false,
            note_refs: Vec::new(),
        }
    }

    fn walk_blocks(
        &mut self,
        blocks: &[BlockContent],
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
        context: StoryContext,
    ) {
        for block in blocks {
            match block {
                BlockContent::Paragraph(p) => self.paragraph(p, pkg, rels, profile, context),
                BlockContent::Table(t) => {
                    self.tables += 1;
                    for detail in &t.props.visible_unsupported {
                        profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
                    }
                    for row in &t.rows {
                        if row.props.hidden {
                            continue;
                        }
                        for detail in &row.props.visible_unsupported {
                            profile.add_reason_deduped(
                                UnsupportedCode::UnknownElement,
                                detail.clone(),
                            );
                        }
                        for cell in &row.cells {
                            for detail in &cell.props.visible_unsupported {
                                profile.add_reason_deduped(
                                    UnsupportedCode::UnknownElement,
                                    detail.clone(),
                                );
                            }
                            self.walk_blocks(
                                &cell.content,
                                pkg,
                                rels,
                                profile,
                                StoryContext::TableCell,
                            );
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
        context: StoryContext,
    ) {
        self.paragraphs += 1;
        let eff_para = self.styles.para_props(&p.props);
        self.has_tab_leaders |= eff_para
            .tabs
            .iter()
            .any(|stop| stop.leader != wml::TabLeader::None)
            && paragraph_has_tab(p);
        if let Err(OfficeError::Unsupported { code, detail }) =
            crate::layout::paragraph::validate_tab_leader_stops(&eff_para.tabs)
        {
            profile.add_reason_deduped(code, detail);
        }
        for detail in &eff_para.visible_unsupported {
            profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
        }
        if let Some(mark) = &eff_para.mark_rpr {
            for detail in &mark.visible_unsupported {
                profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
            }
        }
        if let Some(detail) = eff_para.borders.unsupported_detail(self.theme) {
            profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail);
        }
        if let Some(detail) = eff_para.frame.and_then(|frame| frame.unsupported) {
            profile.add_reason_deduped(UnsupportedCode::TextBox, detail);
        }
        if let Some(numref) = eff_para.numbering
            && let Some(level) = self.numbering.resolve(numref.num_id, numref.ilvl)
        {
            if let NumFmt::Other(v) = &level.num_fmt {
                profile.add_reason_deduped(UnsupportedCode::UnknownElement, format!("numFmt:{v}"));
            }
            let chars: Vec<_> = level.lvl_text.chars().collect();
            for window in chars.windows(2) {
                if window[0] == '%'
                    && let Some(index) = window[1].to_digit(10)
                    && index > 0
                    && let Some(referenced) = self.numbering.resolve(numref.num_id, index - 1)
                    && let NumFmt::Other(value) = referenced.num_fmt
                {
                    profile.add_reason_deduped(
                        UnsupportedCode::UnknownElement,
                        format!("numFmt:{value}"),
                    );
                }
            }
            let eff = self.styles.run_props(&p.props, &level.rpr);
            let family = matching::wml_run_family(&eff.fonts, FontSlot::Ascii, self.theme);
            let mapped = if level.num_fmt == NumFmt::Bullet {
                let symbol_mapped: String = level
                    .lvl_text
                    .chars()
                    .map(|ch| matching::map_symbol_char(&family, ch as u32).unwrap_or(ch))
                    .collect();
                let resolved = matching::resolve(
                    self.font_db,
                    &family,
                    if eff.bold == Some(true) { 700 } else { 400 },
                    eff.italic == Some(true),
                );
                for warning in resolved.warnings {
                    profile.add_warning_deduped(warning);
                }
                if let Some(primary) = resolved.face {
                    let fallback_ids = matching::coverage_fallbacks(
                        self.font_db,
                        primary,
                        if eff.bold == Some(true) { 700 } else { 400 },
                        eff.italic == Some(true),
                    );
                    symbol_mapped
                        .chars()
                        .map(|source| {
                            let rendered = matching::substitute_bullet_char(
                                self.font_db,
                                &fallback_ids,
                                source,
                            )
                            .unwrap_or(source);
                            if rendered != source {
                                profile.add_warning_deduped(Warning::new(
                                    "bullet_substituted",
                                    format!("U+{:04X} → U+{:04X}", source as u32, rendered as u32),
                                ));
                            }
                            rendered
                        })
                        .collect::<String>()
                } else {
                    symbol_mapped
                }
            } else {
                numbering_request_text(&level.lvl_text)
            };
            if !self
                .fonts_used
                .iter()
                .any(|used| used.eq_ignore_ascii_case(&family))
            {
                self.fonts_used.push(family.clone());
            }
            record_coverage(
                &mut self.coverage,
                &family,
                if eff.bold == Some(true) { 700 } else { 400 },
                eff.italic == Some(true),
                &mapped,
            );
        }
        let anchor_count = paragraph_anchor_count(p);
        if anchor_count > 0 && eff_para.frame.is_some() {
            profile.add_reason_deduped(
                UnsupportedCode::AnchoredDrawing,
                "anchored drawing in a framed paragraph",
            );
        }
        for inline in &p.runs {
            self.inline(&p.props, inline, pkg, rels, profile, context);
        }
    }

    fn inline(
        &mut self,
        para: &ParaProps,
        inline: &InlineContent,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
        context: StoryContext,
    ) {
        match inline {
            InlineContent::Bookmark(_) => {}
            InlineContent::Run(run) => self.run(para, run, pkg, rels, profile, context),
            InlineContent::Hyperlink { runs, .. } => {
                for run in runs {
                    self.run(para, run, pkg, rels, profile, context);
                }
            }
            InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                if field.starved && !field.instruction.trim().is_empty() {
                    profile.add_reason_deduped(
                        UnsupportedCode::FieldUnsupported,
                        format!("field without cached result: {}", field.instruction.trim()),
                    );
                }
                for content in &field.content {
                    self.inline(para, content, pkg, rels, profile, context);
                }
            }
            InlineContent::Math(math) => self.math(math, profile),
        }
    }

    fn run(
        &mut self,
        para: &ParaProps,
        run: &wml::Run,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
        context: StoryContext,
    ) {
        let effective = self.styles.run_props(para, &run.props);
        for detail in &effective.visible_unsupported {
            profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
        }
        let mut text = String::new();
        for content in &run.content {
            match content {
                RunContent::Text(t) => text.push_str(t),
                RunContent::Drawing(d) => {
                    for detail in &d.anchor_violations {
                        profile
                            .add_reason_deduped(UnsupportedCode::AnchoredDrawing, detail.clone());
                    }
                    if d.anchor.is_some()
                        && let Some(detail) = context.anchor_detail()
                    {
                        profile.add_reason_deduped(UnsupportedCode::AnchoredDrawing, detail);
                    }
                    if let Some(text_box) = &d.text_box {
                        self.walk_blocks(
                            &text_box.content,
                            pkg,
                            rels,
                            profile,
                            StoryContext::TextBox,
                        );
                    }
                    if let Some(crop) = d.crop
                        && let Err(OfficeError::Unsupported { code, detail }) =
                            crate::drawing::image::validate_crop_fractions(crop)
                    {
                        profile.add_reason_deduped(code, detail);
                    }
                    if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_PICTURE)
                        && !d
                            .drawing
                            .extent_pt
                            .is_some_and(|(width, height)| width > 0.0 && height > 0.0)
                    {
                        profile.add_reason_deduped(
                            UnsupportedCode::MissingRelationshipTarget,
                            "picture drawing has no positive extent",
                        );
                    }
                    if let Some(rel_id) = &d.drawing.blip_rel_id {
                        self.images += 1;
                        self.check_image(rel_id, pkg, rels, profile);
                    }
                    if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_CHART) {
                        if d.drawing.extent_pt.is_none() {
                            profile.add_reason_deduped(
                                UnsupportedCode::Chart,
                                "chart drawing has no extent",
                            );
                        }
                        let result = d
                            .drawing
                            .graphic_rel_id
                            .as_ref()
                            .and_then(|rel_id| rels.get(rel_id))
                            .and_then(|part| pkg.part(part))
                            .and_then(|bytes| std::str::from_utf8(bytes).ok())
                            .map(crate::drawing::chart::Chart::parse);
                        match result {
                            Some(Ok(chart)) => {
                                if let Some((width, height)) = d.drawing.extent_pt
                                    && let Err(error) =
                                        chart.render(self.font_db, self.theme, width, height)
                                {
                                    match error {
                                        OfficeError::Unsupported { code, detail } => {
                                            profile.add_reason_deduped(code, detail)
                                        }
                                        OfficeError::Font(detail) => profile.add_reason_deduped(
                                            UnsupportedCode::MissingGlyph,
                                            detail,
                                        ),
                                        other => profile.add_reason_deduped(
                                            UnsupportedCode::Chart,
                                            format!("chart render validation: {other}"),
                                        ),
                                    }
                                }
                                let mut chart_text = chart.title.clone().unwrap_or_default();
                                chart_text.push_str(
                                    chart.category_axis_title.as_deref().unwrap_or_default(),
                                );
                                chart_text.push_str(
                                    chart.value_axis_title.as_deref().unwrap_or_default(),
                                );
                                for series in &chart.series {
                                    chart_text.push_str(&series.name);
                                    for category in &series.categories {
                                        chart_text.push_str(category);
                                    }
                                }
                                record_coverage(
                                    &mut self.coverage,
                                    "Calibri",
                                    400,
                                    false,
                                    &chart_text,
                                );
                            }
                            Some(Err(OfficeError::Unsupported { code, detail })) => {
                                profile.add_reason_deduped(code, detail);
                            }
                            _ => profile.add_reason_deduped(
                                UnsupportedCode::Chart,
                                "missing chart relationship or cache",
                            ),
                        }
                    }
                    if d.drawing.graphic_uri.as_deref() == Some(crate::oxml::dml::URI_DIAGRAM) {
                        let Some((width, height)) = d.drawing.extent_pt else {
                            profile.add_reason_deduped(
                                UnsupportedCode::SmartArt,
                                "SmartArt drawing has no extent",
                            );
                            continue;
                        };
                        let Some(rel_id) = &d.drawing.graphic_rel_id else {
                            profile.add_reason_deduped(
                                UnsupportedCode::SmartArt,
                                "SmartArt drawing has no data relationship",
                            );
                            continue;
                        };
                        if let Some((code, detail)) = self.diagrams.error(rels, rel_id) {
                            profile.add_reason_deduped(code, detail);
                            continue;
                        }
                        let Some(diagram) = self.diagrams.drawing(rels, rel_id) else {
                            profile.add_reason_deduped(
                                UnsupportedCode::SmartArt,
                                "SmartArt drawing fallback is unavailable",
                            );
                            continue;
                        };
                        if let Err(error) = diagram.render(self.font_db, self.theme, width, height)
                        {
                            match error {
                                OfficeError::Unsupported { code, detail } => {
                                    profile.add_reason_deduped(code, detail)
                                }
                                other => profile.add_reason_deduped(
                                    UnsupportedCode::SmartArt,
                                    format!("SmartArt drawing fallback: {other}"),
                                ),
                            }
                        }
                    }
                }
                RunContent::VmlShape(shape) => {
                    if let Some(detail) = context.anchor_detail() {
                        profile.add_reason_deduped(UnsupportedCode::AnchoredDrawing, detail);
                    }
                    if let Some(rel_id) = &shape.image_rel_id {
                        self.images += 1;
                        self.check_image(rel_id, pkg, rels, profile);
                    }
                    for block in &shape.text_box {
                        self.walk_blocks(
                            std::slice::from_ref(block),
                            pkg,
                            rels,
                            profile,
                            StoryContext::TextBox,
                        );
                    }
                }
                RunContent::OlePreview(shape) => {
                    let Some(rel_id) = &shape.image_rel_id else {
                        profile.add_reason_deduped(
                            UnsupportedCode::OleObject,
                            "OLE object has no preview relationship",
                        );
                        continue;
                    };
                    let Some(target) = rels.get(rel_id) else {
                        profile.add_reason_deduped(
                            UnsupportedCode::MissingRelationshipTarget,
                            format!("OLE preview relationship {rel_id} is missing"),
                        );
                        continue;
                    };
                    match pkg.content_type(target) {
                        Some("image/png" | "image/jpeg" | "image/jpg") => {
                            self.images += 1;
                            self.check_image(rel_id, pkg, rels, profile);
                        }
                        Some("image/x-emf" | "image/emf") => profile.add_reason_deduped(
                            UnsupportedCode::OlePreviewEmf,
                            format!("OLE preview {target} is EMF"),
                        ),
                        Some("image/x-wmf" | "image/wmf" | "image/x-wmf+xml") => {
                            profile.add_reason_deduped(
                                UnsupportedCode::OlePreviewWmf,
                                format!("OLE preview {target} is WMF"),
                            );
                        }
                        Some(content_type) => profile.add_reason_deduped(
                            UnsupportedCode::OleObject,
                            format!("unsupported OLE preview content type {content_type}"),
                        ),
                        None => profile.add_reason_deduped(
                            UnsupportedCode::ImageContentTypeMissing,
                            format!("OLE preview {target} has no content type"),
                        ),
                    }
                }
                RunContent::Math(math) => self.math(math, profile),
                RunContent::NoteReference { kind, id } => {
                    text.push_str(&id.to_string());
                    if !self.note_refs.contains(&(*kind, *id)) {
                        self.note_refs.push((*kind, *id));
                    }
                }
                RunContent::NoteMarker { .. } => {}
                RunContent::Symbol { font, char_code } => {
                    if let Some(ch) = matching::map_symbol_char(font, *char_code) {
                        text.push(ch);
                    } else {
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
            self.record_run_coverage(&effective, &text);
        }
    }

    fn record_run_coverage(&mut self, effective: &RunProps, text: &str) {
        let mut groups: std::collections::BTreeMap<(String, FontSlot), String> =
            std::collections::BTreeMap::new();
        for character in text.chars().filter(|character| !character.is_control()) {
            let slot = slot_for_character(character);
            let family = matching::wml_run_family(&effective.fonts, slot, self.theme);
            groups.entry((family, slot)).or_default().push(character);
        }
        for ((family, slot), source) in groups {
            if !self
                .fonts_used
                .iter()
                .any(|used| used.eq_ignore_ascii_case(&family))
            {
                self.fonts_used.push(family.clone());
            }
            record_coverage_with_slots(
                &mut self.coverage,
                &family,
                if effective.bold == Some(true) {
                    700
                } else {
                    400
                },
                effective.italic == Some(true),
                5,
                &source,
                &BTreeSet::from([slot]),
            );
        }
    }

    fn math(&mut self, math: &crate::oxml::omml::MathContent, profile: &mut DocProfile) {
        self.has_math = true;
        if let Some(detail) = math.unsupported() {
            profile.add_reason_deduped(UnsupportedCode::Equation, detail.to_string());
            return;
        }
        let source = math.source_text();
        if source.is_empty() {
            profile.add_reason_deduped(UnsupportedCode::Equation, "empty equation");
            return;
        }
        record_coverage(
            &mut self.coverage,
            "Cambria Math",
            400,
            source.chars().any(char::is_alphabetic),
            &source,
        );
        if !self.fonts_used.iter().any(|font| font == "Cambria Math") {
            self.fonts_used.push("Cambria Math".to_string());
        }
        self.scripts.merge(scan_scripts(&source));
    }

    /// Validates a required visible image without relying on renderer lookup.
    fn check_image(
        &mut self,
        rel_id: &str,
        pkg: &Package,
        rels: &HashMap<String, String>,
        profile: &mut DocProfile,
    ) {
        let Some(target) = rels.get(rel_id) else {
            profile.add_reason_deduped(
                UnsupportedCode::MissingRelationshipTarget,
                format!("image relationship {rel_id} is missing"),
            );
            return;
        };
        if self.validated_images.contains(target) {
            return;
        }
        let Some(bytes) = pkg.part(target) else {
            profile.add_reason_deduped(
                UnsupportedCode::MissingRelationshipTarget,
                format!("image relationship target {target} is missing"),
            );
            return;
        };
        let Some(content_type) = pkg.content_type(target) else {
            profile.add_reason_deduped(
                UnsupportedCode::ImageContentTypeMissing,
                format!("image relationship target {target} has no content type"),
            );
            return;
        };
        match crate::drawing::image::decode_required(content_type, bytes) {
            Ok((_, pixels)) => {
                if let Err(OfficeError::Unsupported { code, detail }) =
                    crate::drawing::image::add_to_budget(
                        self.validated_images.len(),
                        &mut self.total_image_pixels,
                        pixels,
                    )
                {
                    profile.add_reason_deduped(code, detail);
                    return;
                }
                self.validated_images.insert(target.clone());
            }
            Err(OfficeError::Unsupported { code, detail }) => {
                profile.add_reason_deduped(code, format!("image {target}: {detail}"));
            }
            Err(error) => {
                profile.add_reason_deduped(
                    UnsupportedCode::ImageDecodeFailure,
                    format!("image {target}: {error}"),
                );
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

/// Header/footer references from every section, including mid-document breaks.
fn header_footer_refs(document: &wml::Document) -> Vec<&wml::HdrFtrRef> {
    // Union across every section, not just the final one: a mid-document section
    // break (a paragraph's `sect_break`) carries its own headers/footers, whose
    // content (a chart, complex script, a missing font) must still be profiled.
    let mut sects: Vec<&wml::SectPr> = document
        .body
        .iter()
        .filter_map(|b| match b {
            wml::BlockContent::Paragraph(p) => p.sect_break.as_ref(),
            _ => None,
        })
        .collect();
    sects.push(&document.final_sect);

    let mut references = Vec::new();
    for sect in sects {
        for hf in sect.header_refs.iter().chain(sect.footer_refs.iter()) {
            if !references
                .iter()
                .any(|reference: &&wml::HdrFtrRef| reference.rel_id == hf.rel_id)
            {
                references.push(hf);
            }
        }
    }
    references
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{gdocs_paragraph_docx, minimal_docx, wrap_document};

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
    fn google_docs_conformance_battery_is_supported() {
        let p = run(&gdocs_paragraph_docx(), OfficeFormat::Docx).expect("profile battery");
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
        assert!(p.reasons.is_empty());
        assert_eq!(p.stats.tables, 1);
    }

    #[test]
    fn google_docs_battery_negative_controls_remain_unknown_elements() {
        for (body, detail) in [
            (r#"<w:p><w:r><w:ptab/></w:r></w:p>"#, "w:ptab"),
            (
                r#"<w:p><w:pPr><w:rPr><w:del/></w:rPr></w:pPr></w:p>"#,
                "w:del in w:rPr",
            ),
            (
                r#"<w:p><mc:AlternateContent><mc:Choice Requires="w14"><w:r><w:t>x</w:t></w:r></mc:Choice></mc:AlternateContent></w:p>"#,
                "mc:AlternateContent without mc:Fallback",
            ),
        ] {
            let bytes = minimal_docx(body);
            let profile = match run(&bytes, OfficeFormat::Docx) {
                Ok(profile) => profile,
                Err(error) => DocProfile::from_error(OfficeFormat::Docx, &error),
            };
            assert!(
                profile.reasons.iter().any(|reason| {
                    reason.code == UnsupportedCode::UnknownElement && reason.detail == detail
                }),
                "detail={detail}, reasons={:?}",
                profile.reasons
            );
        }
    }

    #[test]
    fn empty_paragraph_mark_run_properties_are_supported() {
        let p = profile_body(r#"<w:p><w:pPr><w:rPr/></w:pPr></w:p>"#);
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
        assert!(p.reasons.is_empty());
    }

    #[test]
    fn visible_paragraph_mark_deletion_stays_fail_closed() {
        let p = profile_body(r#"<w:p><w:pPr><w:rPr><w:del/></w:rPr></w:pPr></w:p>"#);
        assert!(!p.supported);
        assert!(
            p.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::UnknownElement && reason.detail == "w:del in w:rPr"
            }),
            "reasons: {:?}",
            p.reasons
        );
    }

    #[test]
    fn complex_field_cached_hyperlink_across_paragraphs_is_supported() {
        let p = profile_body(
            r#"
            <w:p>
              <w:r><w:fldChar w:fldCharType="begin"/></w:r>
              <w:r><w:instrText xml:space="preserve"> TOC \h </w:instrText></w:r>
              <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            </w:p>
            <w:p><w:hyperlink w:anchor="heading-1"><w:r><w:t>Heading 1	3</w:t></w:r></w:hyperlink></w:p>
            <w:p><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
            "#,
        );
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
        assert!(
            !p.reasons
                .iter()
                .any(|reason| reason.code == UnsupportedCode::FieldUnsupported)
        );
    }

    #[test]
    fn complex_field_whitespace_only_result_is_starved() {
        let p = profile_body(
            r#"<w:p>
                <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                <w:r><w:instrText> TOC </w:instrText></w:r>
                <w:r><w:fldChar w:fldCharType="separate"/></w:r>
                <w:r><w:t xml:space="preserve">   </w:t></w:r>
                <w:r><w:fldChar w:fldCharType="end"/></w:r>
            </w:p>"#,
        );
        assert!(
            p.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::FieldUnsupported
                    && reason.detail == "field without cached result: TOC"
            }),
            "reasons: {:?}",
            p.reasons
        );
    }

    #[test]
    fn unterminated_complex_field_is_starved_at_story_end() {
        let p = profile_body(
            r#"<w:p>
                <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                <w:r><w:instrText> TOC </w:instrText></w:r>
            </w:p>"#,
        );
        assert!(
            p.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::FieldUnsupported
                    && reason.detail == "field without cached result: TOC"
            }),
            "reasons: {:?}",
            p.reasons
        );
    }

    #[test]
    fn text_box_field_state_is_isolated_from_an_open_body_field() {
        let p = profile_body(
            r#"
            <w:p>
              <w:r><w:fldChar w:fldCharType="begin"/></w:r>
              <w:r><w:instrText> TOC </w:instrText></w:r>
              <w:r><w:fldChar w:fldCharType="separate"/></w:r>
              <w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="457200"/>
                <a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                  <wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                    <wps:txbx><w:txbxContent><w:p>
                      <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                      <w:r><w:instrText> PAGE </w:instrText></w:r>
                      <w:r><w:fldChar w:fldCharType="separate"/></w:r>
                      <w:r><w:t>Box result</w:t></w:r>
                      <w:r><w:fldChar w:fldCharType="end"/></w:r>
                    </w:p></w:txbxContent></wps:txbx>
                  </wps:wsp>
                </a:graphicData></a:graphic>
              </wp:inline></w:drawing></w:r>
            </w:p>
            <w:p><w:r><w:t>Body result</w:t></w:r></w:p>
            <w:p><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
            "#,
        );
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
        assert!(
            !p.reasons
                .iter()
                .any(|reason| reason.code == UnsupportedCode::FieldUnsupported)
        );
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
    fn multicolumn_section_is_supported() {
        let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
            <w:sectPr><w:cols w:num="2"/></w:sectPr>"#;
        let p = profile_body(body);
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
    }

    #[test]
    fn arabic_text_uses_available_coverage_fallback() {
        let bytes = minimal_docx(r#"<w:p><w:r><w:t>مرحبا بالعالم</w:t></w:r></w:p>"#);
        let p = run_with_options(
            &bytes,
            OfficeFormat::Docx,
            &crate::ConvertOptions {
                font_dirs: vec!["../pdf-core/wasm/pkg/fonts".into()],
                ..crate::ConvertOptions::default()
            },
        )
        .expect("profile");
        assert!(p.supported, "unexpected reasons: {:?}", p.reasons);
        assert!(!p.reasons.iter().any(|r| matches!(
            r.code,
            UnsupportedCode::ComplexScript | UnsupportedCode::RtlText
        )));
    }

    #[test]
    fn bundled_subscript_tail_profile_and_layout_boundaries_both_fail_closed() {
        let db = FontDb::with_bundled();
        let mut requests = Vec::new();
        record_coverage(&mut requests, "Calibri", 400, false, "ₕₖₗₘₙₚₛₜ");
        let mut profile = DocProfile::new("docx");
        add_fallback_face_missing_reason(&mut profile, &db, &requests);
        assert!(profile.reasons.iter().any(|reason| {
            reason.code == UnsupportedCode::MissingGlyph && reason.detail.contains("U+2095")
        }));
        assert!(
            profile
                .warnings
                .iter()
                .all(|warning| warning.code != "superscript_synthesized")
        );

        let primary = matching::resolve(&db, "Calibri", 400, false)
            .face
            .expect("bundled primary face");
        let ids = matching::coverage_fallbacks(&db, primary, 400, false);
        let faces: Vec<_> = ids.iter().filter_map(|id| db.face(*id)).collect();
        let shaped = crate::text::shape::shape_with_fallback("ₕₖₗₘₙₚₛₜ", &faces, 12.0, 0.0, false);
        assert!(matches!(
            crate::text::shape::require_glyphs(shaped),
            Err(OfficeError::Unsupported {
                code: UnsupportedCode::MissingGlyph,
                ..
            })
        ));
    }

    #[test]
    fn unsupported_drop_cap_frame_is_typed() {
        let p = profile_body(
            r#"<w:p><w:pPr><w:framePr w:dropCap="drop" w:lines="3"/></w:pPr><w:r><w:t>tb</w:t></w:r></w:p>"#,
        );
        assert!(p.reasons.iter().any(|r| r.code == UnsupportedCode::TextBox));
    }

    #[test]
    fn profile_rejects_mismatched_format_content() {
        // Every format has a profiler, so this never panics. A docx profiled as
        // its own format succeeds; profiled as pptx/xlsx it fails closed with
        // Invalid (it previously returned an empty "supported" verdict, which let
        // convert() emit an invalid 0-page PDF).
        let bytes = minimal_docx("<w:p/>");
        assert_eq!(
            run(&bytes, OfficeFormat::Docx)
                .expect("profile docx")
                .engine,
            "office-core"
        );
        for fmt in [OfficeFormat::Pptx, OfficeFormat::Xlsx] {
            assert!(
                matches!(run(&bytes, fmt), Err(OfficeError::Invalid(_))),
                "expected Invalid for {fmt:?}"
            );
        }
    }

    #[test]
    fn convert_rejects_mismatched_format_content() {
        // A docx handed to the pptx/xlsx path used to open with zero slides/sheets
        // and emit an invalid 0-page PDF. It must now fail closed: the document
        // converts as docx, but the mismatched formats return Invalid rather than
        // a broken file.
        use crate::{ConvertOptions, convert};
        let bytes = minimal_docx("<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        assert!(convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()).is_ok());
        for fmt in [OfficeFormat::Pptx, OfficeFormat::Xlsx] {
            let err = convert(&bytes, fmt, &ConvertOptions::default()).unwrap_err();
            assert!(
                matches!(err, OfficeError::Invalid(_)),
                "expected Invalid for {fmt:?}, got {err:?}"
            );
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
