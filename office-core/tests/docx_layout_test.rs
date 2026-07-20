//! DOCX layout + emission integration tests (agent task T3.9).

use office_core::layout::{Item, LaidOutDocument, Page, WordDoc, layout_docx};
use office_core::testkit::{
    continuous_multicolumn_anchor_docx, continuous_vertically_aligned_page_float_docx,
    cross_page_page_float_band_docx, disjoint_left_page_floats_docx,
    floating_table_block_flow_docx, floating_table_distances_docx,
    floating_table_first_line_indent_docx, floating_table_origin_docx,
    floating_table_side_wrap_docx, full_and_side_page_floats_docx, margin_top_left_float_docx,
    minimal_docx, multicolumn_anchor_docx, non_convergent_page_float_docx,
    opposite_page_floats_docx, overlapping_page_floats_docx, page_center_float_docx,
    page_floats_without_text_window_docx, page_top_bottom_float_docx,
    paragraph_column_top_bottom_float_docx, paragraph_float_with_page_horizontal_origin_docx,
    paragraph_top_bottom_anchor_after_text_docx, paragraph_top_bottom_anchor_order_docx,
    paragraph_top_bottom_anchor_with_after_spacing_docx, single_narrow_page_float_docx,
    stacked_full_then_side_page_floats_docx, stale_page_band_schedule_docx,
    two_paragraph_floats_docx, vertically_aligned_body_page_float_docx, wrap_document, wrap_styles,
    zip_package,
};
use office_core::{
    ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile, profile_with_options,
};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

/// Lays out a docx body and returns the laid-out document.
fn layout_body(body: &str) -> LaidOutDocument {
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    layout_docx(&doc).expect("layout").0
}

fn layout_body_with_styles(body: &str, styles_inner: &str) -> LaidOutDocument {
    let document = wrap_document(body);
    let styles = wrap_styles(styles_inner);
    let content_types = r#"<?xml version="1.0"?>
        <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
        </Types>"#;
    let root_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
        </Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
    ]);
    let doc = WordDoc::open(&bytes, &opts()).expect("open styled docx");
    layout_docx(&doc).expect("layout styled docx").0
}

/// All glyph text on a page, in item (paint) order.
fn page_text(page: &Page) -> String {
    let mut s = String::new();
    for pos in &page.items {
        match &pos.item {
            Item::Glyphs(run) => {
                for g in &run.glyphs {
                    if g.ch != '\0' {
                        s.push(g.ch);
                    }
                }
            }
            Item::SemanticText(run) => {
                for cluster in &run.clusters {
                    s.push_str(&cluster.source);
                }
            }
            _ => {}
        }
    }
    s
}

/// Distinct glyph-run baselines on a page (≈ number of text lines).
fn text_rows(page: &Page) -> usize {
    let mut ys: Vec<i64> = page
        .items
        .iter()
        .filter(|p| matches!(p.item, Item::Glyphs(_)))
        .map(|p| (p.y_pt * 4.0) as i64)
        .collect();
    ys.sort_unstable();
    ys.dedup();
    ys.len()
}

fn glyph_runs(page: &Page) -> Vec<(String, f64, f64, f64)> {
    page.items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some((
                run.glyphs.iter().map(|glyph| glyph.ch).collect(),
                positioned.x_pt,
                positioned.y_pt,
                run.glyphs.iter().map(|glyph| glyph.advance_pt).sum(),
            )),
            _ => None,
        })
        .collect()
}

fn layout_numbered_body(body: &str, numbering_inner: &str) -> LaidOutDocument {
    let ns = office_core::testkit::DOC_NAMESPACES;
    let document =
        format!(r#"<?xml version="1.0"?><w:document{ns}><w:body>{body}</w:body></w:document>"#);
    let numbering =
        format!(r#"<?xml version="1.0"?><w:numbering{ns}>{numbering_inner}</w:numbering>"#);
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ]);
    let doc = WordDoc::open(&bytes, &opts()).expect("open numbered fixture");
    layout_docx(&doc).expect("layout numbered fixture").0
}

fn pdf_page_object_ids(pdf: &[u8]) -> Vec<i32> {
    let text = String::from_utf8_lossy(pdf);
    let mut object = None;
    let mut pages = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 3 && fields[1] == "0" && fields[2] == "obj" {
            object = fields[0].parse::<i32>().ok();
        } else if line.trim() == "/Type /Page"
            && let Some(object) = object
        {
            pages.push(object);
        }
    }
    pages
}

fn pdf_goto_destination_object(pdf: &[u8]) -> Option<i32> {
    String::from_utf8_lossy(pdf).lines().find_map(|line| {
        let destination = line.trim().strip_prefix("/D [")?;
        destination.split_whitespace().next()?.parse().ok()
    })
}

fn pdf_goto_destinations(pdf: &[u8]) -> Vec<(i32, f64)> {
    String::from_utf8_lossy(pdf)
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line
                .trim()
                .strip_prefix("/D [")?
                .split_whitespace()
                .collect();
            let page = fields.first()?.parse().ok()?;
            let xyz = fields.iter().position(|field| *field == "/XYZ")?;
            let top = fields.get(xyz + 2)?.parse().ok()?;
            Some((page, top))
        })
        .collect()
}

fn assert_anchor_rejection_parity(bytes: &[u8], detail: &str) {
    let predicted = profile(bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        predicted.reasons.iter().any(|reason| {
            reason.code == UnsupportedCode::AnchoredDrawing && reason.detail == detail
        }),
        "missing {detail:?} in {:?}",
        predicted.reasons
    );
    assert!(matches!(
        convert(bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::AnchoredDrawing,
            detail: actual,
        }) if actual == detail
    ));
}

#[test]
fn margin_top_left_page_float_reflows_and_releases_body_text() {
    let bytes = margin_top_left_float_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("page float image");
    assert!((image.y_pt - 72.0).abs() <= 0.15, "image_y={}", image.y_pt);
    let runs = glyph_runs(page);
    let first = runs
        .iter()
        .find(|(text, ..)| text.contains("Wrapped body"))
        .expect("first wrapped body run");
    assert!(first.1 >= 72.0 + 77.0 + 9.0 - 0.15, "first={first:?}");
    let below = runs
        .iter()
        .find(|(text, ..)| text.contains("Below float band"))
        .expect("below-band run");
    assert!((below.1 - 72.0).abs() <= 0.15, "below={below:?}");
}

#[test]
fn simultaneous_left_and_right_page_floats_leave_middle_window() {
    let bytes = opposite_page_floats_docx();
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.supported, "reasons: {:?}", predicted.reasons);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let images: Vec<_> = page
        .items
        .iter()
        .filter(|positioned| matches!(positioned.item, Item::Image { .. }))
        .collect();
    assert_eq!(images.len(), 2);
    assert!(images.iter().any(|image| (image.x_pt - 72.0).abs() <= 0.15));
    assert!(
        images
            .iter()
            .any(|image| (image.x_pt - 463.0).abs() <= 0.15)
    );
    let middle = glyph_runs(page)
        .into_iter()
        .find(|(text, ..)| text.contains("Text in the middle"))
        .expect("middle text");
    assert!(middle.1 >= 158.0 - 0.15, "middle={middle:?}");
    assert!(middle.1 + middle.3 <= 454.0 + 0.15, "middle={middle:?}");
}

#[test]
fn overlapping_page_float_band_policies_fail_closed() {
    assert_anchor_rejection_parity(
        &overlapping_page_floats_docx(),
        "overlapping anchored drawing exclusions",
    );
    assert_anchor_rejection_parity(
        &full_and_side_page_floats_docx(),
        "overlapping anchored drawing exclusions",
    );
    assert_anchor_rejection_parity(
        &page_floats_without_text_window_docx(),
        "anchored drawings leave no text window",
    );
}

#[test]
fn page_top_bottom_band_moves_intersecting_text_below_float() {
    let bytes = page_top_bottom_float_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("page float");
    let body = glyph_runs(page)
        .into_iter()
        .find(|(text, ..)| text.contains("Body below"))
        .expect("body text");
    assert!(
        body.2 >= image.y_pt + 72.0 + 9.0 - 0.15,
        "image={image:?}, body={body:?}"
    );
}

#[test]
fn page_relative_center_alignment_resolves_against_page_box() {
    let bytes = page_center_float_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let image = laid.pages[0]
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("centered page float");
    assert!((image.x_pt - 296.0).abs() <= 0.15, "image_x={}", image.x_pt);
    assert!((image.y_pt - 386.0).abs() <= 0.15, "image_y={}", image.y_pt);
}

#[test]
fn column_paragraph_top_bottom_float_reserves_full_width_band() {
    let bytes = paragraph_column_top_bottom_float_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("paragraph float image");
    assert!((image.x_pt - 224.1).abs() <= 0.2, "image_x={}", image.x_pt);
    let below = glyph_runs(page)
        .into_iter()
        .find(|(text, ..)| text.contains("Text below"))
        .expect("text below band");
    assert!(
        below.2 >= image.y_pt + 19.0 - 0.15,
        "image={image:?}, below={below:?}"
    );
}

#[test]
fn paragraph_top_bottom_anchor_after_text_clears_the_same_paragraph_lines() {
    let bytes = paragraph_top_bottom_anchor_after_text_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("paragraph float image");
    let text_runs = glyph_runs(page)
        .into_iter()
        .filter(|(text, ..)| text.contains("Visible") || text.contains("anchored paragraph"))
        .collect::<Vec<_>>();
    assert!(text_rows(page) >= 2, "fixture must wrap: {text_runs:?}");
    let first_baseline = text_runs
        .iter()
        .map(|(_, _, y, _)| *y)
        .min_by(f64::total_cmp)
        .expect("visible text");
    assert!(
        first_baseline >= image.y_pt + 72.0 + 9.0 - 0.15,
        "the paragraph-level image stays at the paragraph top and every same-paragraph text line clears its bottom: image={image:?}, text={text_runs:?}"
    );
}

#[test]
fn paragraph_top_bottom_wrap_is_independent_of_anchor_xml_order() {
    let text = "Same text must have identical baselines on both sides of the source-order pair. "
        .repeat(12);
    let layouts = [false, true].map(|anchor_after_text| {
        let bytes = paragraph_top_bottom_anchor_order_docx(
            anchor_after_text,
            0,
            &text,
            Some("Following paragraph"),
        );
        let doc = WordDoc::open(&bytes, &opts()).expect("open order fixture");
        layout_docx(&doc).expect("layout order fixture").0
    });
    let image_bounds = layouts.each_ref().map(|laid| {
        laid.pages[0]
            .items
            .iter()
            .find_map(|positioned| match positioned.item {
                Item::Image { w_pt, h_pt, .. } => {
                    Some((positioned.x_pt, positioned.y_pt, w_pt, h_pt))
                }
                _ => None,
            })
            .expect("paragraph anchor image")
    });
    assert_eq!(image_bounds[0], image_bounds[1]);
    let baselines = layouts.each_ref().map(|laid| {
        glyph_runs(&laid.pages[0])
            .into_iter()
            .map(|(_, _, y, _)| y)
            .collect::<Vec<_>>()
    });
    assert_eq!(baselines[0], baselines[1]);
}

#[test]
fn positive_y_top_bottom_band_preserves_lines_wholly_above_it() {
    let text = "Line-sized words keep wrapping so the geometric band has text above and below it. "
        .repeat(18);
    let bytes = paragraph_top_bottom_anchor_order_docx(false, 381_000, &text, None);
    let doc = WordDoc::open(&bytes, &opts()).expect("open positive-y fixture");
    let laid = layout_docx(&doc).expect("layout positive-y fixture").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("paragraph anchor image");
    let mut baselines = glyph_runs(page)
        .into_iter()
        .map(|(_, _, y, _)| y)
        .collect::<Vec<_>>();
    baselines.sort_by(f64::total_cmp);
    assert!(
        baselines.iter().filter(|y| **y < image.y_pt).count() >= 2,
        "authored positive y must preserve complete lines above the band: image={image:?}, baselines={baselines:?}"
    );
    let bottom = image.y_pt + 20.0 + 9.0;
    assert!(
        baselines.iter().any(|y| *y >= bottom),
        "the first intersecting line must clear to the band bottom: image={image:?}, baselines={baselines:?}"
    );
    assert!(
        baselines.iter().all(|y| *y < image.y_pt || *y >= bottom),
        "no text baseline may occupy the top-and-bottom band: image={image:?}, baselines={baselines:?}"
    );
}

#[test]
fn anchor_only_paragraph_occupies_through_top_bottom_band() {
    let bytes = paragraph_top_bottom_anchor_order_docx(false, 381_000, "", Some("After band"));
    let doc = WordDoc::open(&bytes, &opts()).expect("open anchor-only fixture");
    let laid = layout_docx(&doc).expect("layout anchor-only fixture").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("paragraph anchor image");
    let after = glyph_runs(page)
        .into_iter()
        .find(|(text, ..)| text.contains("After band"))
        .expect("trailing paragraph");
    assert!(
        after.2 >= image.y_pt + 20.0 + 9.0,
        "the following paragraph must begin below the complete band: image={image:?}, after={after:?}"
    );
}

#[test]
fn top_bottom_band_absorbs_paragraph_after_spacing_inside_its_reserve() {
    let baselines = [0, 200].map(|after_twips| {
        let bytes = paragraph_top_bottom_anchor_with_after_spacing_docx(after_twips);
        let doc = WordDoc::open(&bytes, &opts()).expect("open after-spacing fixture");
        let laid = layout_docx(&doc).expect("layout after-spacing fixture").0;
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text.contains("After spaced band"))
            .expect("trailing paragraph")
            .2
    });

    assert_eq!(baselines[0], baselines[1]);
}

#[test]
fn two_paragraph_relative_floats_fail_profile_convert_parity() {
    assert_anchor_rejection_parity(
        &two_paragraph_floats_docx(),
        "multiple paragraph-relative drawings in one paragraph",
    );
}

#[test]
fn anchor_in_multicolumn_section_fails_profile_convert_parity() {
    assert_anchor_rejection_parity(
        &multicolumn_anchor_docx(),
        "anchored drawing in a multi-column section",
    );
}

#[test]
fn floating_table_wraps_side_text_then_releases_full_width() {
    let bytes = floating_table_side_wrap_docx(2880);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let (laid, warnings) = layout_docx(&doc).expect("layout");
    assert!(
        warnings
            .iter()
            .all(|warning| warning.code != "floating_table_block_fallback"),
        "warnings: {warnings:?}"
    );
    let runs = glyph_runs(&laid.pages[0]);
    let table = runs
        .iter()
        .find(|(text, ..)| text.contains("Floating table"))
        .expect("table text");
    let beside = runs
        .iter()
        .find(|(text, ..)| text.contains("side wrapped"))
        .expect("side-wrapped text");
    assert!(beside.1 >= 72.0 + 144.0 + 9.35 - 0.15, "beside={beside:?}");
    assert!(
        beside.2 < table.2 + 40.0,
        "table={table:?}, beside={beside:?}"
    );
    let below = runs
        .iter()
        .rev()
        .find(|(text, ..)| text.contains("Below floating table"))
        .expect("below-table text");
    assert!((below.1 - 72.0).abs() <= 0.15, "below={below:?}");
}

#[test]
fn floating_table_positioning_ignores_normal_flow_table_indent() {
    let body = r#"<w:tbl><w:tblPr><w:tblW w:w="2160" w:type="dxa"/><w:tblInd w:w="-108" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="0" w:tblpY="1"/></w:tblPr><w:tblGrid><w:gridCol w:w="2160"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>FLOATING INDENT</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let laid = layout_body(body);
    let run = glyph_runs(&laid.pages[0])
        .into_iter()
        .find(|(text, ..)| text == "FLOATING INDENT")
        .expect("floating table text");

    assert!(
        (run.1 - 77.4).abs() <= 0.15,
        "floating frame position comes from tblpPr, not tblInd: {run:?}"
    );
}

#[test]
fn overwide_floating_table_falls_back_with_stable_warning() {
    let table = r#"<w:tbl><w:tblPr><w:tblW w:w="8600" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="0" w:tblpY="1" w:rightFromText="187"/></w:tblPr><w:tblGrid><w:gridCol w:w="8600"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Overwide floating table</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let bytes = minimal_docx(&format!(
        r#"{table}<w:p><w:r><w:t>between</w:t></w:r></w:p>{table}<w:p><w:r><w:t>after</w:t></w:r></w:p>"#
    ));
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("fallback profile");
    assert!(predicted.supported, "reasons: {:?}", predicted.reasons);
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("fallback converts");
    let profile_warnings: Vec<_> = predicted
        .warnings
        .iter()
        .filter(|warning| warning.code == "floating_table_block_fallback")
        .collect();
    let convert_warnings: Vec<_> = output
        .warnings
        .iter()
        .filter(|warning| warning.code == "floating_table_block_fallback")
        .collect();
    assert_eq!(
        profile_warnings.len(),
        1,
        "profile: {:?}",
        predicted.warnings
    );
    assert_eq!(convert_warnings.len(), 1, "convert: {:?}", output.warnings);
    assert_eq!(convert_warnings[0], profile_warnings[0]);
    assert_eq!(output.warnings, predicted.warnings);
}

#[test]
fn page_float_fixpoint_is_bounded_and_fails_typed_on_non_convergence() {
    assert_anchor_rejection_parity(
        &non_convergent_page_float_docx(),
        "page-anchored drawing did not converge",
    );
}

#[test]
fn page_band_fixpoint_uses_current_block_y_instead_of_stale_schedule_y() {
    let bytes = stale_page_band_schedule_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    let image = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("page float");
    let sentinel = glyph_runs(page)
        .into_iter()
        .find(|(text, ..)| text.contains("Schedule sentinel"))
        .expect("sentinel text");
    assert!(
        sentinel.2 > image.y_pt + 55.0 + 9.0,
        "fixture must move sentinel below the band: image={image:?}, sentinel={sentinel:?}"
    );
    assert!(
        (sentinel.1 - 72.0).abs() <= 0.15,
        "a stale schedule kept below-band text narrowed: {sentinel:?}"
    );
}

#[test]
fn disjoint_same_side_page_bands_both_narrow_a_long_paragraph() {
    let bytes = disjoint_left_page_floats_docx();
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.supported, "reasons: {:?}", predicted.reasons);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let page = &laid.pages[0];
    assert_eq!(
        page.items
            .iter()
            .filter(|positioned| matches!(positioned.item, Item::Image { .. }))
            .count(),
        2
    );
    let runs = glyph_runs(page);
    let between = runs
        .iter()
        .find(|(_, _, y, _)| *y > 150.0 && *y < 240.0)
        .expect("text between disjoint bands");
    assert!((between.1 - 72.0).abs() <= 0.15, "between={between:?}");
    let lower = runs
        .iter()
        .find(|(_, _, y, _)| *y > 275.0 && *y < 325.0)
        .expect("text crossing lower band");
    assert!(lower.1 >= 158.0 - 0.15, "lower={lower:?}");
}

#[test]
fn floating_table_block_fallback_preserves_legacy_side_padding_height() {
    let next_y = |bytes: Vec<u8>| {
        let doc = WordDoc::open(&bytes, &opts()).expect("open");
        let (laid, _) = layout_docx(&doc).expect("layout");
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text.contains("Legacy next paragraph"))
            .expect("next paragraph")
            .2
    };
    let no_padding = next_y(floating_table_block_flow_docx(8800, "text", 0));
    let padded = next_y(floating_table_block_flow_docx(8800, "text", 400));
    assert!(
        (padded - no_padding - 20.0).abs() <= 0.15,
        "no_padding={no_padding}, padded={padded}"
    );
}

#[test]
fn absolute_floating_table_preserves_legacy_block_height() {
    let next_y = |bytes: Vec<u8>| {
        let doc = WordDoc::open(&bytes, &opts()).expect("open");
        let (laid, _) = layout_docx(&doc).expect("layout");
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text.contains("Legacy next paragraph"))
            .expect("next paragraph")
            .2
    };
    let no_padding = next_y(floating_table_block_flow_docx(2880, "margin", 0));
    let padded = next_y(floating_table_block_flow_docx(2880, "margin", 400));
    assert!(
        (padded - no_padding - 20.0).abs() <= 0.15,
        "no_padding={no_padding}, padded={padded}"
    );
}

#[test]
fn continuous_column_change_anchor_fails_profile_convert_parity() {
    assert_anchor_rejection_parity(
        &continuous_multicolumn_anchor_docx(),
        "anchored drawing in a multi-column section",
    );
}

#[test]
fn paragraph_float_page_horizontal_offset_uses_page_box() {
    let bytes = paragraph_float_with_page_horizontal_origin_docx(false);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let image = laid.pages[0]
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("page-relative paragraph float");
    assert!((image.x_pt - 18.0).abs() <= 0.15, "image_x={}", image.x_pt);
}

#[test]
fn paragraph_float_page_horizontal_alignment_uses_page_box() {
    let bytes = paragraph_float_with_page_horizontal_origin_docx(true);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let image = laid.pages[0]
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Image { .. }))
        .expect("page-relative paragraph float");
    assert!((image.x_pt - 592.0).abs() <= 0.15, "image_x={}", image.x_pt);
}

#[test]
fn page_float_with_non_top_body_alignment_fails_profile_convert_parity() {
    for alignment in ["center", "bottom"] {
        assert_anchor_rejection_parity(
            &vertically_aligned_body_page_float_docx(alignment),
            "page-anchored drawing with non-top body alignment",
        );
    }
}

#[test]
fn continuous_non_top_body_alignment_page_float_fails_profile_convert_parity() {
    for alignment in ["center", "bottom"] {
        assert_anchor_rejection_parity(
            &continuous_vertically_aligned_page_float_docx(alignment),
            "page-anchored drawing with non-top body alignment",
        );
    }
}

#[test]
fn page_float_band_does_not_bleed_into_paragraph_continuation_page() {
    let bytes = cross_page_page_float_band_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert!(laid.pages.len() >= 2, "pages={}", laid.pages.len());
    let page_one = glyph_runs(&laid.pages[0]);
    let narrowed = page_one
        .iter()
        .find(|(text, x, ..)| text.contains("Continuation") && *x >= 158.0 - 0.15)
        .expect("page-one continuation line crossing the lower band");
    assert!(narrowed.2 > 600.0, "narrowed={narrowed:?}");
    let page_two = glyph_runs(&laid.pages[1]);
    let initial = page_two
        .iter()
        .find(|(text, ..)| text.contains("Continuation"))
        .expect("paragraph continuation on page two");
    assert!(initial.2 < 120.0, "initial={initial:?}");
    assert!(
        (initial.1 - 72.0).abs() <= 0.15,
        "page-one band leaked into page two: {initial:?}"
    );
}

#[test]
fn full_width_clearance_reflows_shifted_lines_against_later_side_band() {
    let bytes = stacked_full_then_side_page_floats_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let shifted = glyph_runs(&laid.pages[0])
        .into_iter()
        .find(|(text, ..)| text.contains("Shifted"))
        .expect("shifted text");
    assert!(shifted.2 >= 141.0 - 0.15, "shifted={shifted:?}");
    assert!(
        shifted.1 >= 158.0 - 0.15,
        "full-width clearance moved a full-width line into the side band: {shifted:?}"
    );
}

#[test]
fn floating_table_side_wrap_uses_resolved_physical_origin() {
    let positions = |origin: &str, x_twips: i32| {
        let bytes = floating_table_origin_docx(origin, x_twips);
        let doc = WordDoc::open(&bytes, &opts()).expect("open");
        let laid = layout_docx(&doc).expect("layout").0;
        let runs = glyph_runs(&laid.pages[0]);
        let table = runs
            .iter()
            .find(|(text, ..)| text.contains("Resolved origin table"))
            .expect("table text");
        let following = runs
            .iter()
            .find(|(text, ..)| text.contains("Resolved-origin"))
            .expect("following text");
        (table.1, following.1, following.2)
    };

    let page_left = positions("page", 0);
    assert!((page_left.0 - 5.4).abs() <= 0.15, "page_left={page_left:?}");
    assert!(
        (page_left.1 - 72.0).abs() <= 0.15 && page_left.2 > 90.0,
        "physical page-left table must keep legacy block flow: {page_left:?}"
    );

    let content_left = positions("page", 1440);
    assert!(
        content_left.1 >= 225.0 - 0.15 && content_left.2 < 90.0,
        "page-origin table resolved to content-left must side-wrap: {content_left:?}"
    );

    let page_right = positions("page", 9360);
    assert!(
        (page_right.0 - 473.4).abs() <= 0.15,
        "page_right={page_right:?}"
    );
    assert!(
        (page_right.1 - 72.0).abs() <= 0.15 && page_right.2 > 90.0,
        "physical page-right table must keep legacy block flow: {page_right:?}"
    );

    let content_right = positions("page", 7920);
    assert!(
        (content_right.1 - 72.0).abs() <= 0.15 && content_right.2 < 90.0,
        "page-origin table resolved to content-right must side-wrap: {content_right:?}"
    );

    for origin in ["margin", "text"] {
        let content_left = positions(origin, 0);
        assert!(
            content_left.1 >= 225.0 - 0.15 && content_left.2 < 90.0,
            "{origin}-origin content-left table must side-wrap: {content_left:?}"
        );
    }
}

#[test]
fn floating_table_side_wrap_uses_the_opposite_edge_distance() {
    let first_flow_run = |side: &str, left_twips: u32, right_twips: u32| {
        let bytes = floating_table_distances_docx(side, left_twips, right_twips, 0);
        let doc = WordDoc::open(&bytes, &opts()).expect("open");
        let laid = layout_docx(&doc).expect("layout").0;
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text.contains("distance flow"))
            .expect("first side-wrapped distance line")
    };

    let left = first_flow_run("left", 800, 100);
    assert!(
        (left.1 - 221.0).abs() <= 0.15,
        "left-hugging inset must use rightFromText=5pt: {left:?}"
    );

    let right = first_flow_run("right", 100, 800);
    assert!((right.1 - 72.0).abs() <= 0.15, "right={right:?}");
    assert!(
        right.1 + right.3 > 370.0,
        "right-hugging window must use leftFromText=5pt: {right:?}"
    );
}

#[test]
fn floating_table_side_wrap_releases_after_bottom_distance() {
    let release_y = |bottom_twips: u32| {
        let bytes = floating_table_distances_docx("left", 0, 0, bottom_twips);
        let doc = WordDoc::open(&bytes, &opts()).expect("open");
        let laid = layout_docx(&doc).expect("layout").0;
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, x, ..)| text.contains("distance flow") && (*x - 72.0).abs() <= 0.15)
            .expect("first full-width line below floating table")
            .2
    };
    let no_bottom = release_y(0);
    let bottom_40pt = release_y(800);
    assert!(
        (bottom_40pt - no_bottom - 40.0).abs() <= 0.5,
        "no_bottom={no_bottom}, bottom_40pt={bottom_40pt}"
    );
}

#[test]
fn side_wrap_preserves_positive_first_line_indent_after_the_float_inset() {
    let bytes = floating_table_first_line_indent_docx();
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let mut wrapped = glyph_runs(&laid.pages[0])
        .into_iter()
        .filter(|(text, ..)| text.contains("first-line indent"));
    let first = wrapped.next().expect("first wrapped line");
    let continuation = wrapped.next().expect("continuation line");

    assert!(
        (first.1 - (225.0 + 21.6)).abs() <= 0.15,
        "first line must apply its 432-twip indent after the float inset: {first:?}"
    );
    assert!(
        (continuation.1 - 225.0).abs() <= 0.15,
        "continuation line must begin at the float inset: {continuation:?}"
    );
}

#[test]
fn one_narrow_side_page_band_is_not_rejected_by_opposite_band_floor() {
    let bytes = single_narrow_page_float_docx();
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(predicted.supported, "reasons: {:?}", predicted.reasons);
    convert(&bytes, OfficeFormat::Docx, &opts()).expect("single side band converts");
}

#[test]
fn converts_real_mock_docx_to_valid_pdf() {
    let bytes = std::fs::read("../mock-files/docx/file1.docx").unwrap();
    let out = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert");
    assert!(out.pdf.starts_with(b"%PDF-"));
    assert!(out.page_count >= 1);
    assert!(out.pdf.len() > 500);
}

#[test]
fn conversion_is_deterministic() {
    let bytes = minimal_docx(r#"<w:p><w:r><w:t>Deterministic output test.</w:t></w:r></w:p>"#);
    let a = convert(&bytes, OfficeFormat::Docx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Docx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "same input must yield byte-identical PDF");
}

#[test]
fn simple_text_lays_out_and_is_selectable() {
    let laid = layout_body(r#"<w:p><w:r><w:t>Hello world</w:t></w:r></w:p>"#);
    assert_eq!(laid.pages.len(), 1);
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("Hello world"), "got {text:?}");
}

#[test]
fn calibri_arabic_uses_registered_cluster_fallback() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/></w:rPr><w:t>مرحبا بالعالم</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..ConvertOptions::default()
    };
    let result = convert(&bytes, OfficeFormat::Docx, &options);
    assert!(result.is_ok(), "Arabic fallback failed: {result:?}");
}

#[test]
fn latin_family_falls_back_for_packaged_unicode_scripts() {
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..ConvertOptions::default()
    };
    for source in [
        "中文 日本語 한국어 漢字",
        "कर्म ภาษาไทย",
        "Ελληνικά Кириллица",
        "⌚️",
    ] {
        let bytes = minimal_docx(&format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/></w:rPr><w:t>{source}</w:t></w:r></w:p>"#,
        ));
        let result = convert(&bytes, OfficeFormat::Docx, &options);
        assert!(result.is_ok(), "fallback failed for {source:?}: {result:?}");
    }
}

#[test]
fn docx_shaped_clusters_extract_complete_logical_source_once() {
    let source = "e\u{301} é لا שָׁ क्षि ก้ ⌚️ 👍🏽 👩‍💻 LTR שלום 123";
    let bytes = minimal_docx(&format!(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/></w:rPr><w:t>{source}</w:t></w:r></w:p>"#
    ));
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let doc = WordDoc::open(&bytes, &options).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let extracted: String = page_text(&laid.pages[0])
        .chars()
        .filter(|character| *character != '\0')
        .collect();
    assert_eq!(extracted, source);
}

#[test]
fn docx_complex_run_before_direct_run_keeps_source_order() {
    let source = "e\u{301} Latin";
    let bytes = minimal_docx(
        r#"<w:p><w:r><w:t>é</w:t></w:r><w:r><w:t xml:space="preserve"> Latin</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let doc = WordDoc::open(&bytes, &options).unwrap();
    let laid = layout_docx(&doc).unwrap().0;

    assert_eq!(page_text(&laid.pages[0]), source);
    assert!(laid.pages[0].items.iter().any(|positioned| {
        matches!(
            &positioned.item,
            Item::Glyphs(run) if run.base_direction == Some(text_core::Direction::Ltr)
        )
    }));
}

#[test]
fn rtl_paragraph_places_hebrew_run_right_of_latin_run() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t>שלום</w:t></w:r><w:r><w:t> Latin</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let semantic: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::SemanticText(run) => Some((
                positioned.x_pt,
                run.clusters
                    .iter()
                    .map(|cluster| cluster.source.as_str())
                    .collect::<String>(),
            )),
            _ => None,
        })
        .collect();
    let hebrew_x = semantic
        .iter()
        .find_map(|(x, text)| text.contains("שלום").then_some(*x));
    let latin_x = semantic
        .iter()
        .find_map(|(x, text)| text.contains("Latin").then_some(*x));
    assert!(
        hebrew_x.is_some() && latin_x.is_some() && hebrew_x > latin_x,
        "Hebrew must be positioned to the right of Latin: {semantic:?}"
    );
}

#[test]
fn long_text_wraps_to_multiple_lines() {
    let words = "word ".repeat(200);
    let body = format!("<w:p><w:r><w:t xml:space=\"preserve\">{words}</w:t></w:r></w:p>");
    let laid = layout_body(&body);
    let rows: usize = laid.pages.iter().map(text_rows).sum();
    assert!(rows > 5, "expected many wrapped lines, got {rows}");
}

#[test]
fn hanging_indent_does_not_expand_the_first_line_past_the_right_text_edge() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:ind w:left="360" w:right="720" w:hanging="360"/></w:pPr><w:r><w:t>Markdown — A lightweight markup language popular for documentation and technical writing.</w:t></w:r></w:p>"#,
    );
    assert_eq!(
        text_rows(&laid.pages[0]),
        2,
        "the hanging area belongs to the label, not to the first line's right edge"
    );
}

#[test]
fn hanging_indent_moves_only_the_first_text_line_into_the_hanging_area() {
    let text = "first line starts at the content edge while continuation lines retain the authored left indent ".repeat(12);
    let laid = layout_body(&format!(
        r#"<w:p><w:pPr><w:ind w:left="720" w:hanging="720"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#,
    ));
    let mut lines = glyph_runs(&laid.pages[0]);
    lines.sort_by(|left, right| left.2.total_cmp(&right.2));
    assert!(lines.len() >= 2, "expected wrapped text: {lines:?}");
    assert!(
        ((lines[1].1 - lines[0].1) - 36.0).abs() < 0.1,
        "w:left=720/w:hanging=720 puts line 1 at the content edge and continuations 36pt right: {lines:?}"
    );
}

#[test]
fn direct_hanging_indent_clears_an_inherited_first_line_indent() {
    let text = "first line starts at the content edge while continuation lines retain the authored left indent ".repeat(12);
    let laid = layout_body_with_styles(
        &format!(
            r#"<w:p><w:pPr><w:ind w:left="720" w:hanging="720"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#,
        ),
        r#"<w:docDefaults><w:pPrDefault><w:pPr><w:ind w:firstLine="432"/></w:pPr></w:pPrDefault></w:docDefaults>"#,
    );
    let mut lines = glyph_runs(&laid.pages[0]);
    lines.sort_by(|left, right| left.2.total_cmp(&right.2));
    assert!(lines.len() >= 2, "expected wrapped text: {lines:?}");
    assert!(
        (lines[0].1 - 72.0).abs() < 0.1,
        "the direct hanging indent must clear inherited firstLine and start at the content edge: {lines:?}"
    );
    assert!(
        ((lines[1].1 - lines[0].1) - 36.0).abs() < 0.1,
        "continuations retain the direct 36pt left indent: {lines:?}"
    );
}

#[test]
fn inherited_first_line_indent_does_not_move_a_right_tab_past_the_paragraph_edge() {
    let laid = layout_body_with_styles(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs></w:pPr><w:r><w:t>Title</w:t><w:tab/><w:t>9</w:t></w:r></w:p>"#,
        r#"<w:docDefaults><w:pPrDefault><w:pPr><w:ind w:firstLine="432"/></w:pPr></w:pPrDefault></w:docDefaults>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let title = runs
        .iter()
        .find(|(text, ..)| text == "Title")
        .expect("title run");
    let digit = runs
        .iter()
        .find(|(text, ..)| text == "9")
        .expect("right-tab digit");
    assert!(
        (title.2 - digit.2).abs() < 0.1,
        "firstLine only moves the initial text; the right-tab digit remains on that line: {runs:?}"
    );
    assert!(
        ((digit.1 + digit.3) - (72.0 + 467.5)).abs() < 0.1,
        "the right tab is paragraph-left-relative: {runs:?}"
    );
}

#[test]
fn left_and_first_line_indents_do_not_shift_a_margin_relative_right_tab() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:ind w:left="240" w:firstLine="192"/><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs></w:pPr><w:r><w:t>Subentry</w:t><w:tab/><w:t>9</w:t></w:r></w:p>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let title = runs
        .iter()
        .find(|(text, ..)| text == "Subentry")
        .expect("subentry run");
    let digit = runs
        .iter()
        .find(|(text, ..)| text == "9")
        .expect("right-tab digit");
    assert!(
        (title.2 - digit.2).abs() < 0.1,
        "paragraph indents must not wrap the margin-relative right-tab digit: {runs:?}"
    );
    assert!(
        ((digit.1 + digit.3) - (72.0 + 467.5)).abs() < 0.1,
        "the tab stop remains relative to the page-margin content origin: {runs:?}"
    );
}

#[test]
fn right_paragraph_border_and_shading_extend_outside_the_text_edge() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:shd w:val="clear" w:fill="D9D9D9"/><w:pBdr><w:right w:val="single" w:sz="4" w:space="4" w:color="000000"/></w:pBdr><w:tabs><w:tab w:val="right" w:pos="9350"/></w:tabs></w:pPr><w:r><w:t>Title</w:t><w:tab/><w:t>9</w:t></w:r></w:p>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let title = runs
        .iter()
        .find(|(text, ..)| text == "Title")
        .expect("title run");
    let digit = runs
        .iter()
        .find(|(text, ..)| text == "9")
        .expect("right-tab digit");
    assert!(
        (title.2 - digit.2).abs() < 0.1,
        "the external right border must not reduce the normal text width: {runs:?}"
    );

    let shading_right = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match positioned.item {
            Item::Rect {
                w_pt,
                fill: Some(_),
                ..
            } => Some(positioned.x_pt + w_pt),
            _ => None,
        })
        .expect("paragraph shading");
    let border_x = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match positioned.item {
            Item::Line {
                dx_pt,
                dy_pt,
                ref stroke,
            } if dx_pt.abs() < 0.01 && dy_pt > 1.0 && (stroke.width_pt - 0.5).abs() < 0.01 => {
                Some(positioned.x_pt)
            }
            _ => None,
        })
        .expect("right paragraph border");
    assert!(
        (shading_right - 544.5).abs() < 0.1,
        "shading reaches the outer border edge: {shading_right}"
    );
    assert!(
        (border_x - 544.25).abs() < 0.1,
        "border center is 4pt outside the 540pt text edge: {border_x}"
    );
}

#[test]
fn scripted_large_run_keeps_the_unscaled_base_em_for_auto_line_spacing() {
    let document = |vert_align: &str| {
        format!(
            r#"<w:p><w:pPr><w:spacing w:line="951" w:lineRule="auto"/></w:pPr><w:r><w:rPr><w:sz w:val="195"/><w:vertAlign w:val="{vert_align}"/></w:rPr><w:t>D</w:t></w:r></w:p><w:p><w:r><w:t>NEXT</w:t></w:r></w:p>"#,
        )
    };
    let scripted = layout_body(&document("subscript"));
    let baseline = layout_body(&document("baseline"));
    let next_y = |page: &Page| {
        glyph_runs(page)
            .into_iter()
            .find(|(text, ..)| text.contains("NEXT"))
            .expect("NEXT glyph run")
            .2
    };
    assert!(
        (next_y(&scripted.pages[0]) - next_y(&baseline.pages[0])).abs() < 0.5,
        "subscript transforms the glyph but must not shrink the authored 97.5pt auto-spaced line box: scripted={:?}, baseline={:?}",
        glyph_runs(&scripted.pages[0]),
        glyph_runs(&baseline.pages[0]),
    );
    let scripted_size = scripted.pages[0]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Glyphs(run)
                if run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>() == "D" =>
            {
                Some(run.size_pt)
            }
            _ => None,
        })
        .expect("scripted D glyph run");
    assert!(
        (scripted_size - 97.5 * 0.65).abs() < 0.01,
        "the rendered glyph itself remains script-scaled: {scripted_size}"
    );
}

#[test]
fn scripted_glyph_position_uses_the_face_os2_y_offset() {
    let document = |vert_align: &str| {
        format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans"/><w:sz w:val="80"/><w:vertAlign w:val="{vert_align}"/></w:rPr><w:t>D</w:t></w:r></w:p>"#,
        )
    };
    let baseline = layout_body(&document("baseline"));
    let subscript = layout_body(&document("subscript"));
    let superscript = layout_body(&document("superscript"));
    let y = |laid: &LaidOutDocument| {
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text == "D")
            .expect("scripted D")
            .2
    };

    let bytes = include_bytes!("../fonts/liberation/LiberationSans-Regular.ttf");
    let face = ttf_parser::Face::parse(bytes, 0).expect("Liberation Sans face");
    let units_per_em = f64::from(face.units_per_em());
    let base_size = 40.0;
    let sub_offset = f64::from(
        face.subscript_metrics()
            .expect("subscript metrics")
            .y_offset,
    ) / units_per_em
        * base_size;
    let super_offset = f64::from(
        face.superscript_metrics()
            .expect("superscript metrics")
            .y_offset,
    ) / units_per_em
        * base_size;

    assert!(
        ((y(&subscript) - y(&baseline)) - sub_offset).abs() < 0.01,
        "subscript position must use OS/2 y_offset: baseline={:?}, subscript={:?}",
        glyph_runs(&baseline.pages[0]),
        glyph_runs(&subscript.pages[0]),
    );
    assert!(
        ((y(&superscript) - y(&baseline)) + super_offset).abs() < 0.01,
        "superscript position must use OS/2 y_offset: baseline={:?}, superscript={:?}",
        glyph_runs(&baseline.pages[0]),
        glyph_runs(&superscript.pages[0]),
    );
}

#[test]
fn two_line_auto_spacing_uses_the_face_hhea_line_gap() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans"/><w:sz w:val="24"/></w:rPr><w:t>FIRST</w:t><w:br/><w:t>SECOND</w:t></w:r></w:p>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let first_y = runs
        .iter()
        .find(|(text, ..)| text == "FIRST")
        .map(|(_, _, y, _)| *y)
        .expect("first line");
    let second_y = runs
        .iter()
        .find(|(text, ..)| text == "SECOND")
        .map(|(_, _, y, _)| *y)
        .expect("second line");
    let expected_natural = (1854.0 + 434.0 + 67.0) / 2048.0 * 12.0;
    assert!(
        ((second_y - first_y) - 1.15 * expected_natural).abs() < 0.01,
        "12pt Liberation Sans with line=276 auto must include its 67-unit hhea lineGap: {runs:?}"
    );
}

#[test]
fn hhea_line_gap_is_reserved_above_the_first_line_baseline() {
    let laid = layout_body(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans"/><w:sz w:val="24"/></w:rPr><w:t>BASELINE</w:t></w:r></w:p>"#,
    );
    let baseline = glyph_runs(&laid.pages[0])
        .into_iter()
        .find(|(text, ..)| text == "BASELINE")
        .map(|(_, _, y, _)| y)
        .expect("baseline run");
    let expected = 72.0 + (1854.0 + 67.0) / 2048.0 * 12.0;
    assert!(
        (baseline - expected).abs() < 0.01,
        "Word places the hhea line gap above the glyph ascent: baseline={baseline}, expected={expected}"
    );
}

#[test]
fn paragraph_shading_and_run_highlight_are_painted_behind_text() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:shd w:val="clear" w:fill="D9D9D9"/></w:pPr><w:r><w:rPr><w:highlight w:val="yellow"/></w:rPr><w:t>highlighted</w:t></w:r></w:p>"#,
    );
    let fills: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match positioned.item {
            Item::Rect {
                fill: Some(fill), ..
            } => Some((positioned.x_pt, positioned.y_pt, fill.rgb())),
            _ => None,
        })
        .collect();

    assert!(
        fills
            .iter()
            .any(|(_, _, fill)| *fill == office_core::oxml::Rgb::new(217, 217, 217)),
        "paragraph shading missing: {fills:?}"
    );
    assert!(
        fills
            .iter()
            .any(|(_, _, fill)| *fill == office_core::oxml::Rgb::new(255, 255, 0)),
        "run highlight missing: {fills:?}"
    );
}

#[test]
fn a_table_fragment_repaints_its_collapsed_top_boundary_on_the_next_page() {
    let rows = (0..6)
        .map(|index| {
            format!(
                r#"<w:tr><w:trPr><w:trHeight w:val="960" w:hRule="exact"/></w:trPr><w:tc><w:p><w:r><w:t>ROW {index}</w:t></w:r></w:p></w:tc></w:tr>"#
            )
        })
        .collect::<String>();
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders><w:top w:val="single" w:sz="8" w:color="4472C4"/><w:left w:val="single" w:sz="8" w:color="4472C4"/><w:bottom w:val="single" w:sz="8" w:color="4472C4"/><w:right w:val="single" w:sz="8" w:color="4472C4"/><w:insideH w:val="single" w:sz="8" w:color="4472C4"/><w:insideV w:val="single" w:sz="8" w:color="4472C4"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="4320"/></w:tblGrid>{rows}</w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="5760"/><w:pgMar w:top="720" w:right="1800" w:bottom="720" w:left="1800"/></w:sectPr>"#
    );
    let laid = layout_body(&body);
    assert_eq!(laid.pages.len(), 2);
    assert!(
        laid.pages[1].items.iter().any(|positioned| {
            matches!(
                positioned.item,
                Item::Line { dx_pt, dy_pt, .. }
                    if dx_pt > 100.0
                        && dy_pt.abs() < 0.01
                        && (positioned.y_pt - 36.5).abs() < 0.1
            )
        }),
        "the continuation fragment must reserve the top border's outer half-stroke"
    );
}

#[test]
fn paragraph_mark_size_controls_empty_paragraph_line_height_only() {
    let following_baseline = |body: &str| {
        let laid = layout_body(body);
        laid.pages[0]
            .items
            .iter()
            .find_map(|positioned| {
                matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
            })
            .expect("following paragraph baseline")
    };
    let default =
        following_baseline(r#"<w:p/><w:p><w:r><w:t>Following paragraph</w:t></w:r></w:p>"#);
    let marked = following_baseline(
        r#"<w:p><w:pPr><w:rPr><w:sz w:val="56"/></w:rPr></w:pPr></w:p><w:p><w:r><w:t>Following paragraph</w:t></w:r></w:p>"#,
    );
    assert!(
        marked > default + 10.0,
        "28pt paragraph mark must make the empty line taller: default={default}, marked={marked}"
    );
}

#[test]
fn complex_field_cached_hyperlink_across_paragraphs_converts_and_renders() {
    let bytes = minimal_docx(
        r#"
        <w:p>
          <w:r><w:fldChar w:fldCharType="begin"/></w:r>
          <w:r><w:instrText xml:space="preserve"> TOC \h </w:instrText></w:r>
          <w:r><w:fldChar w:fldCharType="separate"/></w:r>
        </w:p>
        <w:p><w:hyperlink w:anchor="heading-1"><w:r><w:t>Heading 1 3</w:t></w:r></w:hyperlink></w:p>
        <w:p><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
        "#,
    );
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert cached TOC result");
    assert!(output.page_count >= 1);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert!(page_text(&laid.pages[0]).contains("Heading 1 3"));
}

#[test]
fn near_right_edge_tab_accounts_for_its_trailing_page_number_before_wrapping() {
    let laid = layout_body(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:pos="9350" w:leader="dot"/></w:tabs></w:pPr><w:r><w:t>Contents entry</w:t><w:tab/><w:t>7</w:t></w:r></w:p>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let entry = runs
        .iter()
        .find(|(text, ..)| text.contains("Contents entry"))
        .expect("entry text run");
    let page_number = runs
        .iter()
        .find(|(text, ..)| text == "7")
        .expect("page number run");

    assert!(
        (entry.2 - page_number.2).abs() < 0.01,
        "right-tab text and its page number must remain on one line: {runs:?}"
    );
    assert!(
        (page_number.1 + page_number.3 - (72.0 + 467.5)).abs() < 0.1,
        "page number must end at the authored 9350-twip stop: {page_number:?}"
    );
}

#[test]
fn explicit_page_break_starts_new_page() {
    let body = r#"<w:p><w:r><w:t>Page one</w:t></w:r></w:p>
        <w:p><w:r><w:br w:type="page"/></w:r></w:p>
        <w:p><w:r><w:t>Page two</w:t></w:r></w:p>"#;
    let laid = layout_body(body);
    assert!(laid.pages.len() >= 2, "page break should add a page");
    assert!(page_text(&laid.pages[0]).contains("Page one"));
    assert!(page_text(laid.pages.last().unwrap()).contains("Page two"));
}

#[test]
fn bordered_table_draws_borders_and_text() {
    let body = r#"
        <w:tbl>
          <w:tblPr>
            <w:tblW w:w="6000" w:type="dxa"/>
            <w:tblBorders>
              <w:top w:val="single" w:sz="8" w:color="000000"/>
              <w:left w:val="single" w:sz="8" w:color="000000"/>
              <w:bottom w:val="single" w:sz="8" w:color="000000"/>
              <w:right w:val="single" w:sz="8" w:color="000000"/>
              <w:insideH w:val="single" w:sz="8" w:color="000000"/>
              <w:insideV w:val="single" w:sz="8" w:color="000000"/>
            </w:tblBorders>
          </w:tblPr>
          <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
          <w:tr>
            <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
            <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
          </w:tr>
          <w:tr>
            <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
            <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>"#;
    let laid = layout_body(body);
    let page = &laid.pages[0];
    let lines = page
        .items
        .iter()
        .filter(|p| matches!(p.item, Item::Line { .. }))
        .count();
    assert!(lines >= 4, "expected table border segments, got {lines}");
    let text = page_text(page);
    for cell in ["A1", "B1", "A2", "B2"] {
        assert!(text.contains(cell), "cell {cell} missing from {text:?}");
    }
}

#[test]
fn auto_layout_table_refits_columns_to_shaped_content() {
    let mut rows = String::new();
    for row in 0..3 {
        rows.push_str("<w:tr>");
        for column in 0..3 {
            rows.push_str(&format!(
                "<w:tc><w:p><w:r><w:t>R{row}C{column}</w:t></w:r></w:p></w:tc>"
            ));
        }
        rows.push_str("</w:tr>");
    }
    let body = format!(
        r#"<w:tbl>
          <w:tblPr><w:tblBorders>
            <w:top w:val="single" w:sz="6"/><w:left w:val="single" w:sz="6"/>
            <w:bottom w:val="single" w:sz="6"/><w:right w:val="single" w:sz="6"/>
            <w:insideH w:val="single" w:sz="6"/><w:insideV w:val="single" w:sz="6"/>
          </w:tblBorders></w:tblPr>
          <w:tblGrid><w:gridCol w:w="2600"/><w:gridCol w:w="2600"/><w:gridCol w:w="2600"/></w:tblGrid>
          {rows}
        </w:tbl>"#
    );
    let laid = layout_body(&body);
    let page = &laid.pages[0];
    let mut vertical_edges = page
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Line { dx_pt, dy_pt, .. } if dx_pt.abs() < 0.01 && *dy_pt > 1.0 => {
                Some(positioned.x_pt)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    vertical_edges.sort_by(f64::total_cmp);
    vertical_edges.dedup_by(|left, right| (*left - *right).abs() < 0.01);
    assert_eq!(
        vertical_edges.len(),
        4,
        "vertical edges: {vertical_edges:?}"
    );

    let shaped_advance = page
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Glyphs(run)
                if run.glyphs.iter().map(|glyph| glyph.ch).collect::<String>() == "R0C0" =>
            {
                Some(run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>())
            }
            _ => None,
        })
        .expect("R0C0 shaped glyph run");
    // A bare AutoFit table has Word's 0.5pt implicit side insets. The grid
    // runs border-center to border-center, so the stroke is not added again.
    let expected = shaped_advance + 2.0 * 0.5;
    assert!(
        (expected - 23.988).abs() < 0.01,
        "pinned AutoFit width: {expected}"
    );
    for width in vertical_edges.windows(2).map(|edges| edges[1] - edges[0]) {
        assert!(
            (width - expected).abs() <= 0.5,
            "AutoFit column should be shaped advance + implicit insets: width={width:.3}, expected={expected:.3}, advance={shaped_advance:.3}, edges={vertical_edges:?}"
        );
    }
}

#[test]
fn auto_layout_gate_preserves_tables_with_explicit_cell_widths() {
    let body = r#"<w:tbl>
      <w:tblPr><w:tblW w:w="0" w:type="auto"/><w:tblBorders>
        <w:left w:val="single" w:sz="6"/><w:right w:val="single" w:sz="6"/>
        <w:insideV w:val="single" w:sz="6"/>
      </w:tblBorders></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2600"/><w:gridCol w:w="2600"/></w:tblGrid>
      <w:tr>
        <w:tc><w:tcPr><w:tcW w:w="2600" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr><w:tcW w:w="2600" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>"#;
    let laid = layout_body(body);
    let mut vertical_edges = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Line { dx_pt, dy_pt, .. } if dx_pt.abs() < 0.01 && *dy_pt > 1.0 => {
                Some(positioned.x_pt)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    vertical_edges.sort_by(f64::total_cmp);
    vertical_edges.dedup_by(|left, right| (*left - *right).abs() < 0.01);
    assert_eq!(
        vertical_edges.len(),
        3,
        "vertical edges: {vertical_edges:?}"
    );
    for width in vertical_edges.windows(2).map(|edges| edges[1] - edges[0]) {
        assert!(
            (width - 130.0).abs() < 0.01,
            "explicit tcW must exclude a table from AutoFit: {vertical_edges:?}"
        );
    }
}

#[test]
fn table_flow_includes_outer_border_halves() {
    let document = |borders: &str| {
        format!(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p>
            <w:tbl>
              <w:tblPr>{borders}</w:tblPr>
              <w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>
              <w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr>
            </w:tbl>
            <w:p><w:r><w:t>After</w:t></w:r></w:p>"#
        )
    };
    let plain = layout_body(&document(""));
    let bordered = layout_body(&document(
        r#"<w:tblBorders>
          <w:top w:val="single" w:sz="8" w:color="000000"/>
          <w:bottom w:val="single" w:sz="8" w:color="000000"/>
          <w:left w:val="single" w:sz="8" w:color="000000"/>
          <w:right w:val="single" w:sz="8" w:color="000000"/>
        </w:tblBorders>"#,
    ));
    let baseline = |page: &Page, needle: &str| {
        page.items
            .iter()
            .find_map(|positioned| match &positioned.item {
                Item::Glyphs(run)
                    if run
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.ch)
                        .collect::<String>()
                        .contains(needle) =>
                {
                    Some(positioned.y_pt)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {needle:?} glyph run"))
    };

    let cell_shift = baseline(&bordered.pages[0], "Cell") - baseline(&plain.pages[0], "Cell");
    assert!(
        (cell_shift - 1.0).abs() <= 0.01,
        "the top 1pt border contributes both an inner and outer half; shift={cell_shift}"
    );
    let after_shift = baseline(&bordered.pages[0], "After") - baseline(&plain.pages[0], "After");
    assert!(
        (after_shift - 2.0).abs() <= 0.01,
        "top and bottom 1pt borders must occupy their full outer extents; shift={after_shift}"
    );
}

#[test]
fn nested_table_renders_inside_cell() {
    let body = r#"
        <w:tbl>
          <w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid>
          <w:tr><w:tc>
            <w:tbl>
              <w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>
              <w:tr><w:tc><w:p><w:r><w:t>Nested</w:t></w:r></w:p></w:tc></w:tr>
            </w:tbl>
          </w:tc></w:tr>
        </w:tbl>"#;
    let laid = layout_body(body);
    assert!(page_text(&laid.pages[0]).contains("Nested"));
}

#[test]
fn terminal_empty_cell_paragraph_after_nested_table_is_structural() {
    let document = |leading: &str, terminal: &str| {
        format!(
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="6000"/></w:tblGrid><w:tr><w:tc>{leading}
            <w:tbl><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>NESTED</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            {terminal}</w:tc></w:tr></w:tbl><w:p><w:r><w:t>AFTER</w:t></w:r></w:p>"#,
        )
    };
    let leading = r#"<w:p/>"#;
    let terminal = r#"<w:p><w:pPr><w:spacing w:before="240"/></w:pPr><w:bookmarkStart w:id="7" w:name="tail"/><w:r><w:rPr><w:rtl/></w:rPr></w:r></w:p>"#;
    let with_terminal = layout_body(&document(leading, terminal));
    let without_terminal = layout_body(&document(leading, ""));
    let without_leading = layout_body(&document("", ""));
    let after_y = |laid: &LaidOutDocument| {
        glyph_runs(&laid.pages[0])
            .into_iter()
            .find(|(text, ..)| text == "AFTER")
            .expect("AFTER run")
            .2
    };
    assert!(
        (after_y(&with_terminal) - after_y(&without_terminal)).abs() < 0.1,
        "the schema-required terminal empty paragraph after a nested table is structural"
    );
    assert!(
        after_y(&without_terminal) - after_y(&without_leading) > 10.0,
        "a leading empty cell paragraph remains authored layout content"
    );
}

#[test]
fn nested_vertical_merge_distributes_content_height_across_its_row_span() {
    let laid = layout_body(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="8000"/></w:tblGrid><w:tr><w:tc>
        <w:tbl><w:tblGrid><w:gridCol w:w="4000"/><w:gridCol w:w="4000"/></w:tblGrid>
          <w:tr>
            <w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr>
              <w:p><w:pPr><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>MERGED ONE</w:t></w:r></w:p>
              <w:p><w:pPr><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>MERGED TWO</w:t></w:r></w:p>
            </w:tc>
            <w:tc><w:p><w:pPr><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>ROW ONE</w:t></w:r></w:p></w:tc>
          </w:tr>
          <w:tr>
            <w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc>
            <w:tc><w:p><w:pPr><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>ROW TWO</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>
        </w:tc></w:tr></w:tbl>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let y = |needle: &str| {
        runs.iter()
            .find(|(text, ..)| text.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?}: {runs:?}"))
            .2
    };
    assert!(
        ((y("ROW TWO") - y("ROW ONE")) - 12.0).abs() < 0.1,
        "the two nonmerged one-line cells define two roughly equal 12pt rows: {runs:?}"
    );
    assert!(
        (y("MERGED TWO") - y("ROW TWO")).abs() < 0.1,
        "the merged two-paragraph content spans both rows instead of inflating only row 1: {runs:?}"
    );
}

#[test]
fn footer_distance_bottom_anchors_a_one_line_footer() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body><w:p><w:r><w:t>BODY</w:t></w:r></w:p>
        <w:sectPr><w:footerReference w:type="default" r:id="rId1"/>
        <w:pgSz w:w="12240" w:h="15840"/>
        <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>
        </w:sectPr></w:body></w:document>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let footer = format!(
        r#"<?xml version="1.0"?><w:ftr{ns}><w:p><w:r><w:rPr><w:sz w:val="16"/></w:rPr><w:t>FOOTER</w:t></w:r></w:p></w:ftr>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let content_types = r#"<?xml version="1.0"?>
        <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
        <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>
        </Types>"#;
    let root_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
        </Relationships>"#;
    let doc_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
        </Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
    ]);

    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let footer_baseline = glyph_runs(&laid.pages[0])
        .iter()
        .find(|(text, ..)| text == "FOOTER")
        .map(|(_, _, y, _)| *y)
        .expect("footer glyph run");
    let baseline_from_bottom = laid.pages[0].height_pt - footer_baseline;
    assert!(
        (baseline_from_bottom - 38.0).abs() < 0.75,
        "a 36pt footer distance bottom-anchors the 8pt line; baseline was {baseline_from_bottom:.3}pt from the page bottom"
    );
}

#[test]
fn header_footer_page_field_shows_page_of_total() {
    // A package with a footer that has PAGE / NUMPAGES fields and enough body
    // content to span multiple pages.
    let paras = "<w:p><w:r><w:t>line</w:t></w:r></w:p>".repeat(160);
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>{paras}
        <w:sectPr><w:footerReference w:type="default" r:id="rId1"/>
        <w:pgSz w:w="12240" w:h="15840"/>
        <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>
        </w:sectPr></w:body></w:document>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let footer = format!(
        r#"<?xml version="1.0"?><w:ftr{ns}><w:p>
        <w:r><w:t xml:space="preserve">Page </w:t></w:r>
        <w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        <w:r><w:t xml:space="preserve"> of </w:t></w:r>
        <w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>1</w:t></w:r></w:fldSimple>
        </w:p></w:ftr>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let content_types = r#"<?xml version="1.0"?>
        <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
        <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>
        </Types>"#;
    let root_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
        </Relationships>"#;
    let doc_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
        </Relationships>"#;

    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
    ]);

    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let total = laid.pages.len();
    assert!(total >= 2, "expected multiple pages, got {total}");
    let first = page_text(&laid.pages[0]);
    assert!(first.contains("Page 1 of"), "footer on p1: {first:?}");
    let text_runs: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some((
                run,
                run.glyphs
                    .iter()
                    .filter(|glyph| glyph.ch != '\0')
                    .map(|glyph| glyph.ch)
                    .collect::<String>(),
            )),
            _ => None,
        })
        .filter(|(_, text)| !text.is_empty())
        .collect();
    let separator_run = text_runs
        .iter()
        .find(|(_, text)| text == " of ")
        .map(|(run, _)| *run)
        .expect("literal separator run between PAGE and NUMPAGES");
    assert!(
        separator_run.glyphs.iter().all(|glyph| {
            glyph.ch != '\0' && glyph.y_offset_pt > office_core::text::semantic::SEMANTIC_START_Y
        }),
        "simple footer text should be mapped directly onto painted glyphs"
    );
    assert_eq!(
        text_runs.first().map(|(_, text)| text.as_str()),
        Some("Page "),
        "footer fields must precede body objects in PDF copy/search order"
    );
    assert!(
        first.contains(&total.to_string()),
        "total {total} in footer: {first:?}"
    );
    let last = page_text(laid.pages.last().unwrap());
    assert!(
        last.contains(&format!("Page {total} of {total}")),
        "footer on last page: {last:?}"
    );
}

/// Contract lock: `profile().supported == convert().is_ok()` and, when
/// unsupported, the error code equals the first profile reason code.
#[test]
fn profile_predicts_convert_verdict() {
    let cases: &[(&str, bool)] = &[
        (r#"<w:p><w:r><w:t>plain supported</w:t></w:r></w:p>"#, true),
        (
            "<w:p><w:r><w:t>unassigned \u{0378} is uncovered</w:t></w:r></w:p>",
            false,
        ),
        (
            r#"<w:p><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
            false,
        ),
        (r#"<w:p><w:r><m:oMath/></w:r></w:p>"#, false),
        (
            r#"<w:p><w:pPr><w:framePr w:w="1000"/></w:pPr><w:r><w:t>tb</w:t></w:r></w:p>"#,
            true,
        ),
    ];
    for (body, expect_supported) in cases {
        let bytes = minimal_docx(body);
        let prof = profile(&bytes, OfficeFormat::Docx).expect("profile");
        let conv = convert(&bytes, OfficeFormat::Docx, &opts());
        assert_eq!(
            prof.supported,
            conv.is_ok(),
            "verdict mismatch for {body:?}"
        );
        assert_eq!(
            prof.supported, *expect_supported,
            "expectation for {body:?}"
        );
        if let Err(office_core::OfficeError::Unsupported { code, .. }) = &conv {
            assert_eq!(
                Some(*code),
                prof.reasons.first().map(|r| r.code),
                "code mismatch for {body:?}"
            );
        }
    }
}

#[test]
fn internal_anchor_link_targets_bookmark_page_and_dangling_anchor_is_not_annotated() {
    let linked = minimal_docx(
        r#"<w:p><w:hyperlink w:anchor="target"><w:r><w:t>Jump</w:t></w:r></w:hyperlink><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:bookmarkStart w:id="1" w:name="target"/><w:r><w:t>Destination</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#,
    );
    let predicted = profile(&linked, OfficeFormat::Docx).expect("profile linked anchor");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );
    let output = convert(&linked, OfficeFormat::Docx, &opts()).expect("convert linked anchor");
    let page_ids = pdf_page_object_ids(&output.pdf);
    assert_eq!(page_ids.len(), 2, "page objects: {page_ids:?}");
    assert_eq!(
        pdf_goto_destination_object(&output.pdf),
        Some(page_ids[1]),
        "anchor must resolve to the bookmark's second page"
    );

    let dangling = minimal_docx(
        r#"<w:p><w:hyperlink w:anchor="missing"><w:r><w:t>Dangling</w:t></w:r></w:hyperlink></w:p>"#,
    );
    let dangling_profile = profile(&dangling, OfficeFormat::Docx).expect("profile dangling");
    assert!(dangling_profile.supported);
    let dangling_output =
        convert(&dangling, OfficeFormat::Docx, &opts()).expect("dangling converts");
    assert!(
        !String::from_utf8_lossy(&dangling_output.pdf).contains("/Subtype /Link"),
        "dangling anchors must render text without an annotation"
    );
}

#[test]
fn internal_links_cross_table_boundaries_with_exact_page_and_vertical_destination() {
    let bytes = minimal_docx(
        r#"
        <w:p><w:bookmarkStart w:id="1" w:name="outside"/><w:r><w:t>Outside target </w:t></w:r><w:hyperlink w:anchor="inside"><w:r><w:t>Jump into table</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="1"/><w:r><w:br w:type="page"/></w:r></w:p>
        <w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>First cell line</w:t></w:r></w:p><w:p><w:bookmarkStart w:id="2" w:name="inside"/><w:r><w:t>Inside target </w:t></w:r><w:hyperlink w:anchor="outside"><w:r><w:t>Jump outside</w:t></w:r></w:hyperlink><w:hyperlink w:anchor="missing"><w:r><w:t> Dangling table link</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="2"/></w:p></w:tc></w:tr></w:tbl>
        "#,
    );
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("cross-table links convert");
    let pages = pdf_page_object_ids(&output.pdf);
    assert_eq!(pages.len(), 2, "page objects: {pages:?}");
    let destinations = pdf_goto_destinations(&output.pdf);
    assert_eq!(destinations.len(), 2, "destinations: {destinations:?}");
    assert_eq!(
        destinations[0].0, pages[1],
        "outside source -> table target"
    );
    assert_eq!(
        destinations[1].0, pages[0],
        "table source -> outside target"
    );
    assert!(
        destinations[0].1 < destinations[1].1 - 10.0,
        "the second cell paragraph needs its own lower XYZ target: {destinations:?}"
    );
}

#[test]
fn tab_leaders_are_extraction_visible_glyphs_and_require_a_positive_gap() {
    for (leader, expected) in [("dot", '.'), ("hyphen", '-'), ("underscore", '_')] {
        let body = format!(
            r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="{leader}" w:pos="8640"/></w:tabs></w:pPr><w:r><w:t>Entry</w:t><w:tab/><w:t>12</w:t></w:r></w:p>"#
        );
        let bytes = minimal_docx(&body);
        let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile normal leader");
        assert!(
            predicted.supported,
            "{leader} reasons: {:?}",
            predicted.reasons
        );
        convert(&bytes, OfficeFormat::Docx, &opts()).expect("normal leader converts");
        let laid = layout_body(&body);
        let runs = glyph_runs(&laid.pages[0]);
        let leader_run = runs
            .iter()
            .find(|(text, ..)| !text.is_empty() && text.chars().all(|ch| ch == expected))
            .unwrap_or_else(|| panic!("missing {leader} glyph leader in {runs:?}"));
        assert!(leader_run.3 > 0.0, "leader must occupy the tab gap");
    }

    let no_gap = layout_body(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="120"/></w:tabs></w:pPr><w:r><w:t>Already past stop</w:t><w:tab/><w:t>12</w:t></w:r></w:p>"#,
    );
    assert!(
        glyph_runs(&no_gap.pages[0])
            .iter()
            .all(|(text, ..)| text.is_empty() || !text.chars().all(|ch| ch == '.')),
        "no leader may be painted without a positive authored-stop gap"
    );
}

#[test]
fn tiny_positive_tracked_tab_leader_has_profile_convert_resource_parity() {
    // Carlito's 11pt dot advance minus 50 twips of finite negative tracking is
    // still positive, but narrow enough that a 1440pt stop would need more
    // than the bounded 4096 extraction-visible leader glyphs.
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="28800"/></w:tabs></w:pPr><w:r><w:rPr><w:spacing w:val="-50"/></w:rPr><w:t>x</w:t><w:tab/><w:t>y</w:t></w:r></w:p>"#,
    );
    let expected = "tab leader glyph count exceeds 4096 safety limit";
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::InputTooLarge,
            detail,
        }) if detail == expected
    ));

    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile tracked leader");
    assert_eq!(
        predicted.reasons.len(),
        1,
        "reasons: {:?}",
        predicted.reasons
    );
    assert_eq!(predicted.reasons[0].code, UnsupportedCode::InputTooLarge);
    assert_eq!(predicted.reasons[0].detail, expected);
}

#[test]
fn hostile_tab_leader_gap_fails_closed_before_glyph_materialization() {
    // This finite stop would make the old float-to-usize path derive a
    // saturating, attacker-sized repeat count before collecting the glyph Vec.
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="100000000000000000000"/></w:tabs></w:pPr><w:r><w:t>x</w:t><w:tab/><w:t>y</w:t></w:r></w:p>"#,
    );
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile hostile tab");
    assert_eq!(
        predicted.reasons.len(),
        1,
        "reasons: {:?}",
        predicted.reasons
    );
    assert_eq!(predicted.reasons[0].code, UnsupportedCode::InputTooLarge);
    assert_eq!(
        predicted.reasons[0].detail,
        "tab leader gap exceeds 1440pt safety limit"
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::InputTooLarge,
            detail,
        }) if detail == "tab leader gap exceeds 1440pt safety limit"
    ));

    let negative = minimal_docx(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="-100000000000000000000"/></w:tabs></w:pPr><w:r><w:t>negative</w:t><w:tab/><w:t>safe</w:t></w:r></w:p>"#,
    );
    let negative_profile = profile(&negative, OfficeFormat::Docx).expect("profile negative tab");
    assert!(
        negative_profile.supported,
        "reasons: {:?}",
        negative_profile.reasons
    );
    convert(&negative, OfficeFormat::Docx, &opts()).expect("negative stop converts safely");
}

#[test]
fn unicode_superscript_scientific_notation_has_profile_convert_warning_parity() {
    let bytes = minimal_docx(r#"<w:p><w:r><w:t>10⁻⁹ m</w:t></w:r></w:p>"#);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile superscripts");
    assert!(
        predicted.supported,
        "profile reasons: {:?}",
        predicted.reasons
    );
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert superscripts");
    assert_eq!(output.warnings, predicted.warnings);
    assert!(
        predicted
            .warnings
            .iter()
            .all(|warning| warning.code != "superscript_synthesized")
    );
    let pdf = String::from_utf8_lossy(&output.pdf);
    assert!(!pdf.is_empty());

    let doc = WordDoc::open(&bytes, &opts()).expect("open superscripts");
    let laid = layout_docx(&doc).expect("layout superscripts").0;
    assert!(page_text(&laid.pages[0]).contains("10⁻⁹ m"));
}

#[test]
fn canonical_core_properties_dates_flow_deterministically_into_pdf_metadata() {
    let document = wrap_document(r#"<w:p><w:r><w:t>Dated</w:t></w:r></w:p>"#);
    let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dcterms="http://purl.org/dc/terms/"><dcterms:created>2024-01-02T03:04:05Z</dcterms:created><dcterms:modified>2025-06-07T08:09:10Z</dcterms:modified></cp:coreProperties>"#;
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
    ]);

    let doc = WordDoc::open(&bytes, &opts()).expect("open dated input");
    assert_eq!(doc.dates.created, "D:20240102030405Z");
    assert_eq!(doc.dates.modified, "D:20250607080910Z");
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert dated input");
    let pdf = String::from_utf8_lossy(&output.pdf);
    assert!(pdf.contains("/CreationDate (D:20240102030405)"));
    assert!(pdf.contains("/ModDate (D:20250607080910)"));
}

fn assert_missing_header_footer_target_is_typed(bytes: &[u8], expected_detail: &str) {
    let predicted = profile(bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        predicted.reasons.iter().any(|reason| {
            reason.code == UnsupportedCode::MissingRelationshipTarget
                && reason.detail.contains(expected_detail)
        }),
        "profile did not classify missing header/footer target: {predicted:?}"
    );
    assert!(matches!(
        convert(bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::MissingRelationshipTarget,
            detail,
        }) if detail.contains(expected_detail)
    ));
}

#[test]
fn profile_and_convert_classify_missing_header_relationship_identically() {
    let bytes = minimal_docx(
        r#"<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdMissing"/></w:sectPr>"#,
    );
    assert_missing_header_footer_target_is_typed(&bytes, "rIdMissing");
}

#[test]
fn profile_and_convert_classify_missing_header_part_identically() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body><w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/></w:sectPr></w:body></w:document>"#,
        ns = office_core::testkit::DOC_NAMESPACES,
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
    ]);
    assert_missing_header_footer_target_is_typed(&bytes, "word/header1.xml");
}

#[test]
fn docx_profile_supported_implies_convert_succeeds_for_corpus_and_wave4_fixtures() {
    let mut cases = Vec::new();
    for entry in std::fs::read_dir("../mock-files/docx").expect("mock DOCX corpus") {
        let path = entry.expect("mock DOCX entry").path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("docx") {
            cases.push((path.display().to_string(), std::fs::read(&path).unwrap()));
        }
    }
    for (name, body) in [
        (
            "frame-exact-overflow",
            r#"<w:p><w:pPr><w:framePr w:w="720" w:h="264" w:hRule="exact"/></w:pPr><w:r><w:t>one two three four five</w:t></w:r></w:p>"#,
        ),
        (
            "row-grid-and-fixed-layout",
            r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/></w:tblGrid><w:tr><w:trPr><w:gridBefore w:val="1"/><w:wBefore w:w="1000" w:type="dxa"/></w:trPr><w:tc><w:tcPr><w:tcMar><w:left w:w="200" w:type="dxa"/></w:tcMar><w:vAlign w:val="bottom"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        ),
        (
            "typed-visible-property",
            r#"<w:p><w:r><w:rPr><w:emboss/></w:rPr><w:t>x</w:t></w:r></w:p>"#,
        ),
        (
            "missing-header-relationship",
            r#"<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdMissing"/></w:sectPr>"#,
        ),
    ] {
        cases.push((name.to_string(), minimal_docx(body)));
    }

    assert!(!cases.is_empty(), "DOCX invariant corpus is empty");
    for (name, bytes) in cases {
        let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile DOCX invariant case");
        if predicted.supported {
            let converted = convert(&bytes, OfficeFormat::Docx, &opts());
            assert!(
                converted.is_ok(),
                "profile supported {name}, convert failed: {converted:?}"
            );
        }
    }
}

#[test]
fn missing_glyph_profile_and_convert_agree_on_codepoint() {
    let bytes = minimal_docx("<w:p><w:r><w:t>unassigned \u{0378}</w:t></w:r></w:p>");
    let prof = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        prof.reasons.iter().any(|reason| {
            reason.code == UnsupportedCode::MissingGlyph && reason.detail.contains("U+0378")
        }),
        "profile={prof:?}"
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::MissingGlyph,
            detail,
        }) if detail.contains("U+0378")
    ));
}

#[test]
fn two_sections_use_distinct_page_geometry() {
    let body = r#"
        <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Letter</w:t></w:r></w:p>
        <w:p><w:r><w:t>Landscape A4</w:t></w:r></w:p>
        <w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/></w:sectPr>"#;
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert_eq!(laid.pages.len(), 2);
    assert_eq!(
        (laid.pages[0].width_pt, laid.pages[0].height_pt),
        (612.0, 792.0)
    );
    assert_ne!(
        (laid.pages[0].width_pt, laid.pages[0].height_pt),
        (laid.pages[1].width_pt, laid.pages[1].height_pt)
    );
}

#[test]
fn continuous_section_with_changed_geometry_fails_closed() {
    let body = r#"
        <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Letter</w:t></w:r></w:p>
        <w:p><w:r><w:t>A4</w:t></w:r></w:p>
        <w:sectPr><w:type w:val="continuous"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#;
    let bytes = minimal_docx(body);
    let prof = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(prof.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::UnknownElement
            && reason.detail.contains("continuous section break")
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::UnknownElement,
            detail,
        }) if detail.contains("continuous section break")
    ));
}

#[test]
fn continuous_section_with_same_geometry_stays_on_page() {
    let body = r#"
        <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>First section</w:t></w:r></w:p>
        <w:p><w:r><w:t>Second section</w:t></w:r></w:p>
        <w:sectPr><w:type w:val="continuous"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>"#;
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert_eq!(laid.pages.len(), 1);
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("First section") && text.contains("Second section"));
}

#[test]
fn header_table_text_is_laid_out() {
    let ns = office_core::testkit::DOC_NAMESPACES;
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH"/></w:sectPr></w:body></w:document>"#
    );
    let header = format!(
        r#"<?xml version="1.0"?><w:hdr{ns}><w:tbl><w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>HDRCELL</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:hdr>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let header_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").expect("fixture image");
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
        ("word/_rels/header1.xml.rels", header_rels.as_bytes()),
        ("word/media/image1.png", &image),
    ]);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert!(page_text(&laid.pages[0]).contains("HDRCELL"));
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Image { .. }))
    );
}

#[test]
fn multicolumn_section_renders() {
    let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>
        <w:sectPr><w:cols w:num="2"/></w:sectPr>"#;
    let bytes = minimal_docx(body);
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("multi-column conversion");
    assert!(output.pdf.starts_with(b"%PDF-"));
    // Keep the wrap_document helper exercised.
    assert!(wrap_document("<w:p/>").contains("w:body"));
}

/// Multi-level list counters: deeper levels reset when a shallower level
/// increments (Word restart semantics). Verified via the rendered label glyphs.
#[test]
fn list_counters_reset_on_shallower_level() {
    let ns = office_core::testkit::DOC_NAMESPACES;
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{ns}>
        <w:abstractNum w:abstractNumId="0">
          <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
          <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/></w:lvl>
        </w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let mk = |lvl: u32, t: &str| {
        format!(
            "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{lvl}\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>{t}</w:t></w:r></w:p>"
        )
    };
    let body = format!(
        "{}{}{}{}{}",
        mk(0, "one"), // 1.
        mk(1, "a"),   // a)
        mk(1, "b"),   // b)
        mk(0, "two"), // 2.  → resets level 1
        mk(1, "c"),   // a)  (reset, NOT c)
    );
    let document =
        format!(r#"<?xml version="1.0"?><w:document{ns}><w:body>{body}</w:body></w:document>"#);
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ]);

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    let text: String = laid.pages.iter().map(page_text).collect();
    // Labels: 1. a) b) 2. a)  — the final item resets to "a", proving the reset.
    let a_count = text.matches("a)").count();
    assert_eq!(
        a_count, 2,
        "level-1 counter must reset after '2.': {text:?}"
    );
    assert!(
        text.contains("1.") && text.contains("2."),
        "top-level labels: {text:?}"
    );
    assert!(text.contains("b)"), "second level-1 item is 'b)': {text:?}");
}

#[test]
fn numbering_suffix_positions_item_text_after_the_label() {
    let ns = office_core::testkit::DOC_NAMESPACES;
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>Appendix item</w:t></w:r></w:p>
        </w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    for (name, suffix_xml) in [
        ("omitted", ""),
        ("tab", r#"<w:suff w:val="tab"/>"#),
        ("nothing", r#"<w:suff w:val="nothing"/>"#),
        ("space", r#"<w:suff w:val="space"/>"#),
    ] {
        let numbering = format!(
            r#"<?xml version="1.0"?><w:numbering{ns}>
            <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">
            <w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>{suffix_xml}
            <w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr>
            </w:lvl></w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
            </w:numbering>"#
        );
        let bytes = zip_package(&[
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", root_rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
            ("word/numbering.xml", numbering.as_bytes()),
        ]);
        let doc = WordDoc::open(&bytes, &opts()).expect("open numbered fixture");
        let laid = layout_docx(&doc).expect("layout numbered fixture").0;
        let runs = glyph_runs(&laid.pages[0]);
        let (_, label_x, _, label_width) = runs
            .iter()
            .find(|(text, ..)| text.contains("1."))
            .expect("numbering label");
        let item_x = runs
            .iter()
            .find(|(text, ..)| text.contains("Appendix item"))
            .map(|(_, x, ..)| *x)
            .expect("numbered item text");
        let label_end = label_x + label_width;
        match name {
            "omitted" | "tab" => assert!(
                ((item_x - label_x) - 18.0).abs() <= 0.15,
                "{name} suffix must tab to the 18pt text indent: label_x={label_x}, item_x={item_x}, runs={runs:?}"
            ),
            "nothing" => assert!(
                (item_x - label_end).abs() <= 0.15,
                "nothing suffix must start text at label end: label_end={label_end}, item_x={item_x}, runs={runs:?}"
            ),
            "space" => assert!(
                item_x > label_end + 0.15,
                "space suffix must start text strictly after label end: label_end={label_end}, item_x={item_x}, runs={runs:?}"
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn numbering_level_tab_positions_the_body_at_the_authored_stop() {
    let laid = layout_numbered_body(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Level tab body</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">
        <w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
        <w:pPr><w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs><w:ind w:left="720" w:hanging="360"/></w:pPr>
        </w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let body_x = runs
        .iter()
        .find(|(text, ..)| text.contains("Level tab body"))
        .map(|(_, x, ..)| *x)
        .expect("numbered body");
    assert!(
        (body_x - 144.0).abs() <= 0.15,
        "level w:tab@pos=1440 must place the body 72pt from the text margin: {runs:?}"
    );
}

#[test]
fn numbering_justification_aligns_at_the_hanging_anchor_before_the_suffix_tab() {
    for (justification, expected) in [("left", "start"), ("center", "center"), ("right", "end")] {
        let laid = layout_numbered_body(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Roman body</w:t></w:r></w:p>"#,
            &format!(
                r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">
                <w:start w:val="8"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%1."/><w:lvlJc w:val="{justification}"/>
                <w:pPr><w:ind w:left="1152" w:hanging="360"/></w:pPr>
                </w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#
            ),
        );
        let runs = glyph_runs(&laid.pages[0]);
        let (_, label_x, _, label_width) = runs
            .iter()
            .find(|(text, ..)| text == "viii.")
            .expect("Roman numbering label");
        let body_x = runs
            .iter()
            .find(|(text, ..)| text.contains("Roman body"))
            .map(|(_, x, ..)| *x)
            .expect("Roman body");
        let hanging_anchor = 72.0 + 57.6 - 18.0;
        let actual = match expected {
            "start" => *label_x,
            "center" => label_x + label_width / 2.0,
            "end" => label_x + label_width,
            _ => unreachable!(),
        };
        assert!(
            (actual - hanging_anchor).abs() <= 0.15,
            "{justification} label must align its {expected} at the hanging anchor: {runs:?}"
        );
        assert!(
            (body_x - 129.6).abs() <= 0.15,
            "the default tab suffix must target the 57.6pt text indent: {runs:?}"
        );
        if justification == "right" {
            assert!(
                body_x > label_x + label_width + 17.8,
                "a right label must preserve the full hanging gap: {runs:?}"
            );
        }
    }
}

#[test]
fn numbering_direct_indent_and_tabs_override_level_geometry() {
    let laid = layout_numbered_body(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:tabs><w:tab w:val="left" w:pos="1800"/></w:tabs><w:ind w:left="1800" w:hanging="360"/></w:pPr><w:r><w:t>Direct geometry</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">
        <w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
        <w:pPr><w:tabs><w:tab w:val="left" w:pos="1440"/></w:tabs><w:ind w:left="720" w:hanging="180"/></w:pPr>
        </w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let label_x = runs
        .iter()
        .find(|(text, ..)| text == "1.")
        .map(|(_, x, ..)| *x)
        .expect("numbering label");
    let body_x = runs
        .iter()
        .find(|(text, ..)| text.contains("Direct geometry"))
        .map(|(_, x, ..)| *x)
        .expect("numbered body");
    assert!(
        (label_x - 144.0).abs() <= 0.15 && (body_x - 162.0).abs() <= 0.15,
        "direct left/hanging/tab values must override the numbering level: {runs:?}"
    );
}

#[test]
fn numbering_label_overflow_uses_the_next_default_tab_stop() {
    let laid = layout_numbered_body(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Overflow body</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0">
        <w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%1.%1."/>
        <w:pPr><w:ind w:left="360" w:hanging="180"/></w:pPr>
        </w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let body_x = runs
        .iter()
        .find(|(text, ..)| text.contains("Overflow body"))
        .map(|(_, x, ..)| *x)
        .expect("overflow body");
    assert!(
        (body_x - 108.0).abs() <= 0.15,
        "an overflowing label must advance from the 18pt target to the 36pt default stop: {runs:?}"
    );
}

#[test]
fn numbering_symbol_face_does_not_increase_body_line_pitch() {
    let laid = layout_numbered_body(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr><w:r><w:t>First body</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr><w:r><w:t>Second body</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="●"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr><w:rFonts w:ascii="Noto Sans Symbols"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let first_y = runs
        .iter()
        .find(|(text, ..)| text.contains("First body"))
        .map(|(_, _, y, _)| *y)
        .expect("first body");
    let second_y = runs
        .iter()
        .find(|(text, ..)| text.contains("Second body"))
        .map(|(_, _, y, _)| *y)
        .expect("second body");

    let body_only = layout_numbered_body(
        r#"<w:p><w:pPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr><w:r><w:t>First control</w:t></w:r></w:p>
        <w:p><w:pPr><w:spacing w:line="276" w:lineRule="auto"/></w:pPr><w:r><w:t>Second control</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="●"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr><w:rFonts w:ascii="Noto Sans Symbols"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let control_runs = glyph_runs(&body_only.pages[0]);
    let control_first_y = control_runs
        .iter()
        .find(|(text, ..)| text.contains("First control"))
        .map(|(_, _, y, _)| *y)
        .expect("first control body");
    let control_second_y = control_runs
        .iter()
        .find(|(text, ..)| text.contains("Second control"))
        .map(|(_, _, y, _)| *y)
        .expect("second control body");
    assert!(
        ((second_y - first_y) - (control_second_y - control_first_y)).abs() <= 0.01,
        "numbering ink must not replace the paragraph body's line box: numbered={runs:?}, control={control_runs:?}"
    );
}

#[test]
fn numbering_text_label_contributes_to_body_line_pitch() {
    let laid = layout_numbered_body(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First body</w:t></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second body</w:t></w:r></w:p>"#,
        r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="[%1]"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr><w:sz w:val="40"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
    );
    let runs = glyph_runs(&laid.pages[0]);
    let first_y = runs
        .iter()
        .find(|(text, ..)| text.contains("First body"))
        .map(|(_, _, y, _)| *y)
        .expect("first body");
    let second_y = runs
        .iter()
        .find(|(text, ..)| text.contains("Second body"))
        .map(|(_, _, y, _)| *y)
        .expect("second body");
    assert!(
        second_y - first_y >= 23.0,
        "a textual numbering label remains part of the first-line box: {runs:?}"
    );
}

/// Y-S3: headers on a *mid-document* section must be profiled, not just the final
/// section's. A two-section doc with Arabic (complex script) in the first
/// section's header must fail closed; before the all-section union fix, that
/// header escaped the profile entirely.
#[test]
fn mid_section_header_content_is_profiled() {
    let ns = office_core::testkit::DOC_NAMESPACES;
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>
        <w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/>
          <w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Section one</w:t></w:r></w:p>
        <w:p><w:r><w:t>Section two</w:t></w:r></w:p>
        <w:sectPr><w:headerReference w:type="default" r:id="rIdH2"/>
          <w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>"#
    );
    let hdr1 = format!(
        r#"<?xml version="1.0"?><w:hdr{ns}><w:p><w:r><w:t>مرحبا بالعالم</w:t></w:r></w:p></w:hdr>"#
    );
    let hdr2 = format!(
        r#"<?xml version="1.0"?><w:hdr{ns}><w:p><w:r><w:t>Header two</w:t></w:r></w:p></w:hdr>"#
    );
    let content_types = r#"<?xml version="1.0"?>
        <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
        <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
        <Default Extension="xml" ContentType="application/xml"/>
        <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
        <Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
        <Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
        </Types>"#;
    let root_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
        </Relationships>"#;
    let doc_rels = r#"<?xml version="1.0"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
        <Relationship Id="rIdH2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/>
        </Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/header1.xml", hdr1.as_bytes()),
        ("word/header2.xml", hdr2.as_bytes()),
    ]);
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..ConvertOptions::default()
    };
    let prof = profile_with_options(&bytes, OfficeFormat::Docx, &options).expect("profile");
    assert!(
        prof.supported,
        "registered fallback should cover header Arabic: {:?}",
        prof.reasons
    );
    convert(&bytes, OfficeFormat::Docx, &options).expect("header Arabic converts");
}

#[test]
fn uncached_field_uses_field_unsupported_code() {
    let bytes = minimal_docx(r#"<w:p><w:fldSimple w:instr=" PAGE "/></w:p>"#);
    let prof = profile(&bytes, OfficeFormat::Docx).unwrap();
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::FieldUnsupported)
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::FieldUnsupported,
            ..
        })
    ));
}

/// Regression: CFF-flavoured faces (the pan-CJK Noto Sans CJK OTFs) must emit
/// as CIDFontType0 + FontFile3 with an embedded encoding CMap — never as
/// CIDFontType2 + FontFile2, which is invalid for CFF programs.
#[test]
fn docx_cjk_text_embeds_cff_faces_as_cid_font_type0() {
    let bytes = minimal_docx("<w:p><w:r><w:t>你好世界 CJK regression</w:t></w:r></w:p>");
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let prof = profile_with_options(&bytes, OfficeFormat::Docx, &options).unwrap();
    assert!(prof.supported, "CJK profile miss: {:?}", prof.reasons);
    let pdf = convert(&bytes, OfficeFormat::Docx, &options).unwrap().pdf;

    let contains = |needle: &[u8]| pdf.windows(needle.len()).any(|window| window == needle);
    assert!(
        contains(b"CIDFontType0C"),
        "CFF program must embed as FontFile3/CIDFontType0C"
    );
    assert!(
        contains(b"FontFile3"),
        "CFF descriptor must reference FontFile3"
    );
    assert!(
        contains(b"/DocYantra-CFF-Encoding"),
        "CFF Type0 font must map codes through the embedded encoding CMap"
    );
}
