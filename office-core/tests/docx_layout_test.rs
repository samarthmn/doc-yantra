//! DOCX layout + emission integration tests (agent task T3.9).

use office_core::layout::{Item, LaidOutDocument, Page, WordDoc, layout_docx};
use office_core::testkit::{
    continuous_multicolumn_anchor_docx, continuous_vertically_aligned_page_float_docx,
    cross_page_page_float_band_docx, disjoint_left_page_floats_docx,
    floating_table_block_flow_docx, floating_table_distances_docx, floating_table_origin_docx,
    floating_table_side_wrap_docx, full_and_side_page_floats_docx, margin_top_left_float_docx,
    minimal_docx, multicolumn_anchor_docx, non_convergent_page_float_docx,
    opposite_page_floats_docx, overlapping_page_floats_docx, page_center_float_docx,
    page_floats_without_text_window_docx, page_top_bottom_float_docx,
    paragraph_column_top_bottom_float_docx, paragraph_float_with_page_horizontal_origin_docx,
    single_narrow_page_float_docx, stacked_full_then_side_page_floats_docx,
    stale_page_band_schedule_docx, two_paragraph_floats_docx,
    vertically_aligned_body_page_float_docx, wrap_document, zip_package,
};
use office_core::{ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

/// Lays out a docx body and returns the laid-out document.
fn layout_body(body: &str) -> LaidOutDocument {
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    layout_docx(&doc).expect("layout").0
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
fn overwide_floating_table_falls_back_with_stable_warning() {
    let bytes = floating_table_side_wrap_docx(8600);
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("fallback converts");
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| { warning.code == "floating_table_block_fallback" }),
        "warnings: {:?}",
        output.warnings
    );
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
        <w:p><w:hyperlink w:anchor="heading-1"><w:r><w:t>Heading 1	3</w:t></w:r></w:hyperlink></w:p>
        <w:p><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
        "#,
    );
    let output = convert(&bytes, OfficeFormat::Docx, &opts()).expect("convert cached TOC result");
    assert!(output.page_count >= 1);
    let doc = WordDoc::open(&bytes, &opts()).expect("open");
    let laid = layout_docx(&doc).expect("layout").0;
    assert!(page_text(&laid.pages[0]).contains("Heading 1\t3"));
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
    let prof = profile(&bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        prof.supported,
        "registered fallback should cover header Arabic: {:?}",
        prof.reasons
    );
    convert(&bytes, OfficeFormat::Docx, &opts()).expect("header Arabic converts");
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
