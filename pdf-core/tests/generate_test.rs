//! Integration tests for pdf_core::generate — assert real behavior on the
//! output bytes by reloading with lopdf (content streams are uncompressed).

#![cfg(feature = "pdfium")]

use std::io::Cursor;

use lopdf::{Document, Object};
use pdf_core::PdfCoreError;
use pdf_core::extract::extract_text;
use pdf_core::generate::{
    DocOptions, PageSize, csv_to_pdf, images_to_pdf, markdown_to_pdf, text_to_pdf,
};
use pdf_core::raster::{RenderOptions, render_page};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn load(bytes: &[u8]) -> Document {
    Document::load_mem(bytes).expect("generated PDF should reload with lopdf")
}

fn page_ids(doc: &Document) -> Vec<lopdf::ObjectId> {
    doc.get_pages().values().copied().collect()
}

/// Concatenated (uncompressed) content stream bytes of page `idx` (0-based).
fn page_content(doc: &Document, idx: usize) -> Vec<u8> {
    let ids = page_ids(doc);
    doc.get_page_content(ids[idx]).expect("page content")
}

fn page_content_str(doc: &Document, idx: usize) -> String {
    String::from_utf8_lossy(&page_content(doc, idx)).into_owned()
}

/// All `N Tf` font sizes in the content stream, in order of appearance.
fn tf_sizes(content: &str) -> Vec<f64> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let mut sizes = Vec::new();
    for w in tokens.windows(3) {
        if w[2] == "Tf"
            && let Ok(size) = w[1].parse::<f64>()
        {
            sizes.push(size);
        }
    }
    sizes
}

/// Y coordinates of all `x y Td` text-positioning ops.
fn td_y_positions(content: &str) -> Vec<f64> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let mut ys = Vec::new();
    for w in tokens.windows(3) {
        if w[2] == "Td"
            && let Ok(y) = w[1].parse::<f64>()
        {
            ys.push(y);
        }
    }
    ys
}

/// All string literals shown via `(...) Tj` in a content stream, unescaped.
fn show_strings(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let mut s = String::new();
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'\\' if i + 1 < bytes.len() => {
                        s.push(bytes[i + 1] as char);
                        i += 2;
                    }
                    b'(' => {
                        depth += 1;
                        s.push('(');
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            s.push(')');
                        }
                        i += 1;
                    }
                    c => {
                        s.push(c as char);
                        i += 1;
                    }
                }
            }
            out.push(s);
        } else {
            i += 1;
        }
    }
    out
}

/// (x, y, text) of every BT..ET block: the `Td` position plus shown string.
fn text_ops(content: &str) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    for block in content.split("BT").skip(1) {
        let block = block.split("ET").next().unwrap_or("");
        let toks: Vec<&str> = block.split_whitespace().collect();
        let mut pos = None;
        for w in toks.windows(3) {
            if w[2] == "Td"
                && let (Ok(x), Ok(y)) = (w[0].parse::<f64>(), w[1].parse::<f64>())
            {
                pos = Some((x, y));
                break;
            }
        }
        if let (Some((x, y)), Some(text)) = (pos, show_strings(block).into_iter().next()) {
            out.push((x, y, text));
        }
    }
    out
}

/// Show strings across all pages of the document.
fn all_show_strings(doc: &Document) -> Vec<String> {
    (0..doc.get_pages().len())
        .flat_map(|i| show_strings(&page_content_str(doc, i)))
        .collect()
}

/// Darkest pixel luminance of a rendered PNG (255 = blank white page).
fn png_min_luminance(png: &[u8]) -> u8 {
    let img = image::load_from_memory(png).expect("decode rendered PNG");
    img.to_rgb8()
        .pixels()
        .map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8)
        .min()
        .unwrap_or(255)
}

/// BaseFont names of all fonts registered in the page's /Resources.
fn page_base_fonts(doc: &Document, idx: usize) -> Vec<String> {
    let ids = page_ids(doc);
    doc.get_page_fonts(ids[idx])
        .expect("page fonts")
        .values()
        .filter_map(|font| font.get(b"BaseFont").and_then(|o| o.as_name()).ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// text_to_pdf
// ---------------------------------------------------------------------------

#[test]
fn text_wraps_long_paragraph_into_multiple_lines() {
    let paragraph =
        "The DocKosha data room keeps every artifact versioned and watermarked. ".repeat(8);
    let bytes = text_to_pdf(&paragraph, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);

    // Wrapping means several text lines at distinct descending y positions.
    let ys = td_y_positions(&content);
    assert!(
        ys.len() >= 4,
        "expected >=4 wrapped lines, got {} ({content})",
        ys.len()
    );
    let mut distinct = ys.clone();
    distinct.dedup();
    assert!(distinct.len() >= 4, "y positions should differ: {ys:?}");
    for pair in ys.windows(2) {
        assert!(pair[1] < pair[0], "lines must descend: {ys:?}");
    }
    // Actual text made it into the stream.
    assert!(content.contains("DocKosha"), "content: {content}");
}

#[test]
fn text_paginates_and_renders_title() {
    let para = "Retention policies apply to every workspace surface without exception.";
    let text = vec![para; 200].join("\n\n");
    let opts = DocOptions {
        title: Some("Compliance Report".to_string()),
        ..DocOptions::default()
    };
    let bytes = text_to_pdf(&text, &opts).unwrap();
    let doc = load(&bytes);
    let n_pages = doc.get_pages().len();
    assert!(n_pages >= 2, "expected >=2 pages, got {n_pages}");

    let first = page_content_str(&doc, 0);
    assert!(
        first.contains("Compliance Report"),
        "title missing: {first}"
    );
    assert!(first.contains("Retention policies"));
    // Title uses Helvetica-Bold, registered on page 1.
    let fonts = page_base_fonts(&doc, 0);
    assert!(
        fonts.iter().any(|f| f == "Helvetica-Bold"),
        "expected Helvetica-Bold in page resources, got {fonts:?}"
    );
    // Last page still has body text.
    let last = page_content_str(&doc, n_pages - 1);
    assert!(last.contains("Retention policies"));
}

#[test]
fn text_maps_typography_to_ascii() {
    let text = "Smart \u{201C}quotes\u{201D} and an em\u{2014}dash plus caf\u{00E9}.";
    let bytes = text_to_pdf(text, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    assert!(content.contains("\\\"quotes\\\"") || content.contains("\"quotes\""));
    assert!(content.contains("em--dash"), "content: {content}");
    assert!(
        content.contains("caf?"),
        "non-ASCII should become '?': {content}"
    );
}

// ---------------------------------------------------------------------------
// csv_to_pdf
// ---------------------------------------------------------------------------

#[test]
fn csv_fixture_renders_table() {
    let csv_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample.csv"
    ))
    .unwrap();
    let bytes = csv_to_pdf(&csv_bytes, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    assert!(!doc.get_pages().is_empty());

    let content = page_content_str(&doc, 0);
    assert!(content.contains("Region"), "header cell missing: {content}");
    assert!(content.contains("DocKosha"), "body cell missing: {content}");
    // Header is bold and body regular — both Helvetica faces present.
    let fonts = page_base_fonts(&doc, 0);
    assert!(fonts.iter().any(|f| f == "Helvetica-Bold"), "{fonts:?}");
    assert!(fonts.iter().any(|f| f == "Helvetica"), "{fonts:?}");
}

#[test]
fn csv_repeats_header_on_every_page() {
    let mut csv = String::from("AlphaHeader,BetaHeader,GammaHeader\n");
    for i in 0..150 {
        csv.push_str(&format!("row{i}a,row{i}b,row{i}c\n"));
    }
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let n_pages = doc.get_pages().len();
    assert!(
        n_pages >= 2,
        "150 rows must paginate, got {n_pages} page(s)"
    );
    for idx in 0..n_pages {
        let content = page_content_str(&doc, idx);
        assert!(
            content.contains("AlphaHeader"),
            "header not repeated on page {idx}"
        );
    }
    // First and last data rows survived pagination.
    assert!(page_content_str(&doc, 0).contains("row0a"));
    assert!(page_content_str(&doc, n_pages - 1).contains("row149a"));
}

#[test]
fn csv_empty_input_is_invalid() {
    let err = csv_to_pdf(b"", &DocOptions::default()).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "{err:?}");
}

#[test]
fn csv_wide_cell_wraps_fully_without_data_loss() {
    // Regression (parity): long cells used to be truncated to one ellipsized
    // line. A 600-char cell fits within the 8-line cap and must render
    // completely, wrapped across multiple lines of its column.
    let long = "x".repeat(600);
    let csv = format!("Name,Description\nitem,{long}\n");
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let strings = show_strings(&content);
    let x_total: usize = strings
        .iter()
        .map(|s| s.chars().filter(|&c| c == 'x').count())
        .sum();
    assert_eq!(
        x_total, 600,
        "every cell character must render: {strings:?}"
    );
    let wrapped_lines = strings.iter().filter(|s| s.len() > 20).count();
    assert!(
        wrapped_lines >= 3,
        "cell must wrap over multiple lines, got {wrapped_lines}"
    );
    assert!(
        !content.contains(&long),
        "600-char cell must not be emitted as a single line"
    );
}

#[test]
fn csv_two_thousand_char_cell_wraps_then_ellipsizes() {
    // Regression: a 2000-char cell produces a multi-line row; the bulk of
    // what fits in the 8-line cap renders, and the cut is marked "...".
    let cell = "abcdefghij".repeat(200);
    let csv = format!("Name,Description\nitem,{cell}\n");
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);

    let cell_lines: Vec<(f64, String)> = text_ops(&content)
        .into_iter()
        .filter(|(_, _, s)| {
            s.len() > 20
                && s.trim_end_matches("...")
                    .chars()
                    .all(|c| c.is_ascii_lowercase())
        })
        .map(|(_, y, s)| (y, s))
        .collect();
    assert!(
        cell_lines.len() >= 5,
        "expected a multi-line row, got {} lines",
        cell_lines.len()
    );
    // Lines sit on distinct descending baselines.
    for pair in cell_lines.windows(2) {
        assert!(
            pair[1].0 < pair[0].0,
            "cell lines must descend: {cell_lines:?}"
        );
    }
    let total: usize = cell_lines.iter().map(|(_, s)| s.len()).sum();
    assert!(
        total >= 600,
        "most of the fitting content must render, got {total} chars"
    );
    assert!(
        cell_lines.last().unwrap().1.ends_with("..."),
        "truncated cell must end with ellipsis: {cell_lines:?}"
    );
    // And the same content is recoverable via real text extraction.
    let extracted = extract_text(&bytes).expect("extract_text");
    let extracted_cell: usize = extracted
        .split_whitespace()
        .filter(|w| w.len() > 20)
        .map(|w| w.trim_end_matches("...").len())
        .sum();
    assert!(
        extracted_cell >= 600,
        "extraction must recover the wrapped cell, got {extracted_cell} chars"
    );
}

#[test]
fn csv_forty_columns_shrinks_font_and_keeps_every_cell_readable() {
    // Regression (CRITICAL): 25+ column CSVs used to render EVERY cell as a
    // bare "..." (total data loss). Column floors + whole-table font
    // auto-shrink must keep all 40 columns readable.
    let ncols = 40;
    let header: Vec<String> = (0..ncols).map(|i| format!("h{i:02}")).collect();
    let row1: Vec<String> = (0..ncols).map(|i| format!("d{i:02}")).collect();
    let row2: Vec<String> = (0..ncols).map(|i| format!("e{i:02}")).collect();
    let csv = format!(
        "{}\n{}\n{}\n",
        header.join(","),
        row1.join(","),
        row2.join(",")
    );
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let strings = show_strings(&content);

    // No cell may collapse to a bare ellipsis.
    assert!(
        strings.iter().all(|s| s != "..."),
        "no cell may render as bare '...': {strings:?}"
    );
    // Every header and data cell is present in the content stream.
    for cell in header.iter().chain(row1.iter()).chain(row2.iter()) {
        assert!(
            strings.iter().any(|s| s.contains(cell.as_str())),
            "cell {cell} missing from content stream"
        );
    }
    // The table font auto-shrank, but never below the 4pt floor.
    let sizes = tf_sizes(&content);
    assert!(!sizes.is_empty());
    assert!(
        sizes.iter().all(|&s| (4.0..10.0).contains(&s)),
        "table font must shrink into [4, 10): {sizes:?}"
    );
    // Real text extraction sees the headers and data.
    let extracted = extract_text(&bytes).expect("extract_text");
    for probe in ["h00", "h17", "h39", "d25", "e39"] {
        assert!(extracted.contains(probe), "{probe} missing: {extracted}");
    }
    // And the page rasterizes with actual ink on it.
    let png = render_page(&bytes, 0, &RenderOptions::default()).unwrap();
    assert!(
        png_min_luminance(&png) < 128,
        "rendered page must not be blank"
    );
}

#[test]
fn csv_extreme_doc_options_are_clamped_not_panicking() {
    // Regression: base_font_size_pt > ~100.8 used to hit f64::clamp with
    // min > max and panic. Out-of-range options now clamp up front.
    let csv = b"alpha,beta\none,two\n";
    let opts = DocOptions {
        base_font_size_pt: 200.0,
        ..DocOptions::default()
    };
    let bytes = csv_to_pdf(csv, &opts).expect("huge font size must clamp, not panic");
    let doc = load(&bytes);
    let sizes = tf_sizes(&page_content_str(&doc, 0));
    assert!(
        sizes.iter().all(|&s| s <= 72.0 + 0.01),
        "font sizes must clamp to <=72pt: {sizes:?}"
    );

    // Oversized margins clamp instead of erroring.
    let opts = DocOptions {
        margin_pt: 100_000.0,
        ..DocOptions::default()
    };
    csv_to_pdf(csv, &opts).expect("huge margin must clamp, not error");

    // The other generators accept clamped extremes too.
    let opts = DocOptions {
        base_font_size_pt: 200.0,
        margin_pt: 5_000.0,
        ..DocOptions::default()
    };
    text_to_pdf("hello", &opts).expect("text_to_pdf with extreme options");
    markdown_to_pdf("# hi\n\nbody\n", &opts).expect("markdown_to_pdf with extreme options");

    // Non-finite / non-positive inputs are still rejected.
    let opts = DocOptions {
        base_font_size_pt: f64::NAN,
        ..DocOptions::default()
    };
    assert!(matches!(
        csv_to_pdf(csv, &opts).unwrap_err(),
        PdfCoreError::InvalidInput(_)
    ));
    let opts = DocOptions {
        margin_pt: -1.0,
        ..DocOptions::default()
    };
    assert!(matches!(
        csv_to_pdf(csv, &opts).unwrap_err(),
        PdfCoreError::InvalidInput(_)
    ));
}

// ---------------------------------------------------------------------------
// markdown_to_pdf
// ---------------------------------------------------------------------------

#[test]
fn markdown_fixture_renders_document() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample.md"
    ))
    .unwrap();
    let bytes = markdown_to_pdf(&md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let n_pages = doc.get_pages().len();
    assert!(n_pages >= 1);

    let all: String = (0..n_pages).map(|i| page_content_str(&doc, i)).collect();
    assert!(all.contains("DocKosha Security Overview"), "H1 missing");
    assert!(
        all.contains("security@dockosha.com"),
        "trailing paragraph missing"
    );
    // List items (bulleted and numbered) are present.
    assert!(
        all.contains("Owner-level workspace control"),
        "bullet item missing"
    );
    assert!(
        all.contains("Email OTP verification"),
        "ordered item missing"
    );
    assert!(all.contains("(1.)"), "ordered marker '1.' missing: {all}");
    assert!(all.contains("(-)"), "bullet marker '-' missing");
    // Code block uses a Courier font resource on some page.
    let courier_page =
        (0..n_pages).any(|i| page_base_fonts(&doc, i).iter().any(|f| f == "Courier"));
    assert!(courier_page, "code block must register Courier");
    assert!(all.contains("createLink"), "code block text missing");
}

#[test]
fn markdown_heading_sizes_strictly_decrease() {
    let md = "# Alpha\n\n## Beta\n\n### Gamma\n";
    let opts = DocOptions::default(); // base 10pt
    let bytes = markdown_to_pdf(md, &opts).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let sizes = tf_sizes(&content);
    assert_eq!(
        sizes.len(),
        3,
        "expected 3 Tf ops, got {sizes:?} in {content}"
    );
    assert!(
        sizes[0] > sizes[1] && sizes[1] > sizes[2],
        "heading sizes must strictly decrease H1->H3: {sizes:?}"
    );
    // Exact scales from the spec: base * 2.0 / 1.6 / 1.35.
    assert!((sizes[0] - 20.0).abs() < 0.01, "{sizes:?}");
    assert!((sizes[1] - 16.0).abs() < 0.01, "{sizes:?}");
    assert!((sizes[2] - 13.5).abs() < 0.01, "{sizes:?}");
}

#[test]
fn markdown_styles_code_and_rule_render() {
    let md = "Plain **bold** and *italic* with `inline_code_xyz`.\n\n---\n\n\
              ```\nlet answer = 42;\n```\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    assert!(content.contains("bold"));
    assert!(content.contains("italic"));
    assert!(content.contains("inline_code_xyz"));
    assert!(content.contains("let answer = 42;"));
    // Horizontal rule -> a stroked line (l ... S), code block bg -> re ... f.
    let tokens: Vec<&str> = content.split_whitespace().collect();
    assert!(
        tokens.contains(&"l"),
        "expected line op for rule: {content}"
    );
    assert!(tokens.contains(&"S"), "expected stroke op for rule");
    assert!(tokens.contains(&"re"), "expected rect op: {content}");
    assert!(tokens.contains(&"f"), "expected fill op for code bg");
    let fonts = page_base_fonts(&doc, 0);
    assert!(fonts.iter().any(|f| f == "Helvetica-Bold"), "{fonts:?}");
    assert!(fonts.iter().any(|f| f == "Helvetica-Oblique"), "{fonts:?}");
    assert!(fonts.iter().any(|f| f == "Courier"), "{fonts:?}");
}

#[test]
fn markdown_paginates_and_ignores_images() {
    let mut md = String::from("# Long Doc\n\n![ignored alt](https://example.com/x.png)\n\n");
    for i in 0..250 {
        md.push_str(&format!(
            "Paragraph number {i} about DocYantra internals.\n\n"
        ));
    }
    let bytes = markdown_to_pdf(&md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    assert!(doc.get_pages().len() >= 2);
    let all: String = (0..doc.get_pages().len())
        .map(|i| page_content_str(&doc, i))
        .collect();
    assert!(
        !all.contains("ignored alt"),
        "image alt text must be skipped"
    );
    assert!(all.contains("Paragraph number 249"));
}

#[test]
fn markdown_nested_list_items_stay_on_separate_indented_lines() {
    // Regression (MAJOR): child items used to fuse onto the parent's text
    // line ("level1 item textlevel2 item text") and ordered numbering broke.
    let md = "- alpha one\n  1. beta two\n     - gamma three\n  2. delta four\n- epsilon five\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let ops = text_ops(&content);

    let find = |text: &str| -> (f64, f64) {
        ops.iter()
            .find(|(_, _, s)| s == text)
            .map(|(x, y, _)| (*x, *y))
            .unwrap_or_else(|| panic!("item {text:?} missing as its own line: {ops:?}"))
    };
    let alpha = find("alpha one");
    let beta = find("beta two");
    let gamma = find("gamma three");
    let delta = find("delta four");
    let epsilon = find("epsilon five");

    // Each item on its own (descending) baseline.
    let ys = [alpha.1, beta.1, gamma.1, delta.1, epsilon.1];
    for pair in ys.windows(2) {
        assert!(pair[1] < pair[0], "items must descend line by line: {ys:?}");
    }
    // Per-depth indent: 18pt per level from the 54pt margin.
    assert!((alpha.0 - 72.0).abs() < 0.5, "depth-1 x: {}", alpha.0);
    assert!((beta.0 - 90.0).abs() < 0.5, "depth-2 x: {}", beta.0);
    assert!((gamma.0 - 108.0).abs() < 0.5, "depth-3 x: {}", gamma.0);
    assert!((delta.0 - 90.0).abs() < 0.5, "depth-2 x: {}", delta.0);
    assert!((epsilon.0 - 72.0).abs() < 0.5, "depth-1 x: {}", epsilon.0);

    // Markers render per level: bullets and correctly numbered items.
    let strings = show_strings(&content);
    for marker in ["-", "1.", "2."] {
        assert!(
            strings.iter().any(|s| s == marker),
            "marker {marker:?} missing: {strings:?}"
        );
    }
    // No fused item text anywhere.
    assert!(
        strings
            .iter()
            .all(|s| !(s.contains("one") && s.contains("two"))),
        "items must not fuse: {strings:?}"
    );
}

#[test]
fn markdown_html_blocks_render_their_visible_text() {
    // Regression (MAJOR parity): raw HTML blocks were silently dropped,
    // losing human-visible content.
    let md = "Intro paragraph.\n\n<div class=\"box\">\nVisible secret &amp; friends\n</div>\n\n\
              Mid <b>inline kept</b> text.\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);

    assert!(
        strings
            .iter()
            .any(|s| s.contains("Visible secret & friends")),
        "HTML block inner text must render (entities decoded): {strings:?}"
    );
    assert!(
        strings.iter().any(|s| s.contains("inline kept")),
        "inline HTML inner text must survive: {strings:?}"
    );
    // Tags and attributes are stripped, not rendered.
    assert!(
        strings
            .iter()
            .all(|s| !s.contains("<div") && !s.contains("class=") && !s.contains("</b>")),
        "tags must be stripped: {strings:?}"
    );
    // Surrounding markdown still flows normally.
    assert!(strings.iter().any(|s| s.contains("Intro paragraph.")));
}

#[test]
fn markdown_gfm_table_renders_as_table_not_raw_pipes() {
    // Regression (MINOR): GFM tables used to render as literal
    // pipe-delimited paragraphs ("| Name | Qty | |------|-----| ...").
    let md = "Before table.\n\n| Name | Qty |\n|------|-----|\n| foo | 1 |\n| bar | 2 |\n\n\
              After table.\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let strings = show_strings(&content);

    // Header and body cells render as individual table cells.
    for cell in ["Name", "Qty", "foo", "bar", "1", "2"] {
        assert!(
            strings.iter().any(|s| s == cell),
            "cell {cell:?} missing: {strings:?}"
        );
    }
    // No literal pipe characters anywhere.
    assert!(
        strings.iter().all(|s| !s.contains('|')),
        "raw pipes must not render: {strings:?}"
    );
    // Header band (filled rect) and header rule (stroke) are drawn.
    let tokens: Vec<&str> = content.split_whitespace().collect();
    assert!(tokens.contains(&"re"), "header band rect expected");
    assert!(tokens.contains(&"f"), "header band fill expected");
    assert!(tokens.contains(&"S"), "header rule stroke expected");
    // Header is bold; body is regular.
    let fonts = page_base_fonts(&doc, 0);
    assert!(fonts.iter().any(|f| f == "Helvetica-Bold"), "{fonts:?}");
    assert!(fonts.iter().any(|f| f == "Helvetica"), "{fonts:?}");
    // Surrounding paragraphs still render in order.
    assert!(strings.iter().any(|s| s.contains("Before table.")));
    assert!(strings.iter().any(|s| s.contains("After table.")));
}

// ---------------------------------------------------------------------------
// images_to_pdf
// ---------------------------------------------------------------------------

fn make_png_rgba(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 200])
    });
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn make_jpeg_rgb(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([((x + y) % 256) as u8, 64, 32])
    });
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

fn media_box(doc: &Document, idx: usize) -> Vec<f64> {
    let ids = page_ids(doc);
    let page = doc.get_dictionary(ids[idx]).expect("page dictionary");
    let mb = page.get(b"MediaBox").expect("MediaBox present");
    let arr = mb.as_array().expect("MediaBox array");
    arr.iter()
        .map(|o| match o {
            Object::Integer(i) => *i as f64,
            Object::Real(r) => *r as f64,
            _ => panic!("unexpected MediaBox entry"),
        })
        .collect()
}

#[test]
fn images_one_page_per_image_with_requested_size() {
    let png = make_png_rgba(100, 50);
    let jpeg = make_jpeg_rgb(60, 120);
    let opts = DocOptions {
        page_size: PageSize::A4,
        ..DocOptions::default()
    };
    let bytes = images_to_pdf(&[&png, &jpeg], &opts).unwrap();
    let doc = load(&bytes);
    assert_eq!(doc.get_pages().len(), 2, "one page per image");

    let (w, h) = PageSize::A4.dimensions_pt();
    for idx in 0..2 {
        let mb = media_box(&doc, idx);
        assert_eq!(mb.len(), 4);
        assert!((mb[0]).abs() < 0.01 && (mb[1]).abs() < 0.01);
        assert!((mb[2] - w).abs() < 0.01, "page {idx} width: {mb:?}");
        assert!((mb[3] - h).abs() < 0.01, "page {idx} height: {mb:?}");
    }

    // Each page draws its image XObject, scaled to fit inside the margins
    // with aspect ratio preserved (transform op: "sw 0 0 sh x y cm").
    let avail_w = w - 2.0 * opts.margin_pt;
    let avail_h = h - 2.0 * opts.margin_pt;
    let expected = [(100.0_f64, 50.0_f64), (60.0, 120.0)];
    for (idx, (iw, ih)) in expected.iter().enumerate() {
        let content = page_content_str(&doc, idx);
        assert!(content.contains("/Im0 Do"), "page {idx}: {content}");
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let cm = tokens
            .windows(7)
            .find(|w| w[6] == "cm")
            .unwrap_or_else(|| panic!("no cm op on page {idx}: {content}"));
        let sw: f64 = cm[0].parse().unwrap();
        let sh: f64 = cm[3].parse().unwrap();
        assert!(sw <= avail_w + 0.1 && sh <= avail_h + 0.1, "fits margins");
        let scale = (avail_w / iw).min(avail_h / ih);
        assert!((sw - iw * scale).abs() < 0.5, "page {idx} sw={sw}");
        assert!((sh - ih * scale).abs() < 0.5, "page {idx} sh={sh}");
        // Centered.
        let x: f64 = cm[4].parse().unwrap();
        let y: f64 = cm[5].parse().unwrap();
        assert!((x - (w - sw) / 2.0).abs() < 0.5, "centered x");
        assert!((y - (h - sh) / 2.0).abs() < 0.5, "centered y");
    }
}

#[test]
fn images_embed_raw_rgb_xobject() {
    let png = make_png_rgba(10, 4);
    let bytes = images_to_pdf(&[&png], &DocOptions::default()).unwrap();
    let doc = load(&bytes);

    // Find the image XObject stream and validate its dictionary + raw size.
    let mut found = false;
    for (_, obj) in doc.objects.iter() {
        if let Object::Stream(stream) = obj {
            let dict = &stream.dict;
            let is_image = dict
                .get(b"Subtype")
                .and_then(|o| o.as_name())
                .map(|n| n == b"Image")
                .unwrap_or(false);
            if !is_image {
                continue;
            }
            found = true;
            assert_eq!(dict.get(b"Width").unwrap().as_i64().unwrap(), 10);
            assert_eq!(dict.get(b"Height").unwrap().as_i64().unwrap(), 4);
            assert!(dict.get(b"Filter").is_err(), "raw samples: no Filter");
            assert_eq!(
                stream.content.len(),
                10 * 4 * 3,
                "raw RGB8 samples, uncompressed"
            );
        }
    }
    assert!(found, "no image XObject in output");
}

#[test]
fn images_zero_inputs_rejected() {
    let err = images_to_pdf(&[], &DocOptions::default()).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "{err:?}");
}

#[test]
fn images_garbage_bytes_rejected() {
    let err = images_to_pdf(&[b"not an image".as_slice()], &DocOptions::default()).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "{err:?}");
}
