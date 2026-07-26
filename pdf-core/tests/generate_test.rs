//! Integration tests for pdf_core::generate — assert real behavior on the
//! output bytes by reloading with lopdf (content streams are uncompressed).

#![cfg(feature = "pdfium")]

use std::io::Cursor;

use lopdf::{Document, Object};
use pdf_core::PdfCoreError;
use pdf_core::embed_font::register_font;
use pdf_core::extract::extract_text;
use pdf_core::generate::{
    DocOptions, PageSize, csv_to_pdf, images_to_pdf, markdown_to_pdf, text_to_pdf,
};
use pdf_core::raster::{RenderOptions, render_page};

#[test]
fn hostile_csv_column_count_returns_typed_limit_without_panic() {
    let input = std::iter::repeat_n("x", 100_000)
        .collect::<Vec<_>>()
        .join(",");
    let started = std::time::Instant::now();
    let result = std::panic::catch_unwind(|| csv_to_pdf(input.as_bytes(), &DocOptions::default()));
    let error = result.expect("must not panic").expect_err("column limit");
    assert!(matches!(error, PdfCoreError::ResourceLimit(ref message) if
        message.contains("limit")));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn hostile_markdown_nesting_returns_typed_limit_without_panic() {
    let mut markdown = String::new();
    for depth in 0..500 {
        markdown.push_str(&"  ".repeat(depth));
        markdown.push_str("- item\n");
    }
    let result = std::panic::catch_unwind(|| markdown_to_pdf(&markdown, &DocOptions::default()));
    let error = result.expect("must not panic").expect_err("nesting limit");
    assert!(matches!(error, PdfCoreError::InvalidInput(ref message) if message.contains("limit")));
}

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
fn text_preserves_winansi_typography() {
    let text = "Smart \u{201C}quotes\u{201D} and an em\u{2014}dash plus caf\u{00E9}.";
    let bytes = text_to_pdf(text, &DocOptions::default()).unwrap();
    let extracted = extract_text(&bytes).expect("extract WinAnsi typography");

    assert!(extracted.contains("“quotes”"), "got {extracted:?}");
    assert!(extracted.contains("em—dash"), "got {extracted:?}");
    assert!(extracted.contains("café"), "got {extracted:?}");
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
    // Legacy CSV HTML used one regular monospace face for header and body.
    let fonts = page_base_fonts(&doc, 0);
    assert_eq!(fonts, ["Courier"], "{fonts:?}");
}

#[test]
fn csv_repeats_header_on_every_page() {
    let mut csv = String::from("AlphaHeader,BetaHeader,GammaHeader\n");
    for i in 0..150 {
        csv.push_str(&format!("row{i}a,row{i}b,row{i}c\n"));
    }
    let opts = DocOptions {
        page_size: PageSize::LetterLandscape,
        ..DocOptions::default()
    };
    let bytes = csv_to_pdf(csv.as_bytes(), &opts).unwrap();
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
fn csv_preserves_unbreakable_cell_atoms() {
    // Regression: proportional column squeezing followed by character-level
    // wrapping split IDs, dates, joined identifiers, and ordinary words.
    // Content-aware minima must keep each of these atoms in one text run.
    let csv = "order_id,order_date,customer,company,city,country,sku,product,\
               channel,qty,unit_price_eur,line_total_eur\n\
               SO-30412,2026-04-01,Grace Whitfield,\"Meridian Foods, Ltd.\",\
               Leeds,Switzerland,KV-3315,\"Chai latte mix, 400 g tin\",\
               distributor,24,11.25,270.00\n";
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);
    for atom in [
        "SO-30412",
        "2026-04-01",
        "Whitfield",
        "Switzerland",
        "KV-3315",
        "distributor",
        "unit_price_eur",
        "line_total_eur",
        "11.25",
        "270.00",
    ] {
        assert!(
            strings.iter().any(|text| text == atom),
            "{atom:?} must render without a mid-token split: {strings:?}"
        );
    }
}

#[test]
fn csv_wide_numeric_table_shrinks_before_splitting_atoms() {
    // Regression: every week_start date and several numeric values in a
    // 14-column table were character-split. The whole table must shrink
    // together until cell atoms fit, while header wrapping remains limited
    // to natural underscore boundaries.
    let csv = "week_start,active_users,new_signups,activation_pct,retention_d7_pct,\
               mrr_usd,expansion_usd,churn_pct,nps,tickets,avg_reply_hrs,\
               uptime_pct,deploys,error_rate_pct\n\
               2026-02-01,18883,480,37.9,27.5,216797.3,8660.04,1.76,51,188,\
               4.9,99.914,29,0.22\n";
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);
    for atom in ["week_start", "2026-02-01", "216797.3", "8660.04", "99.914"] {
        assert!(
            strings.iter().any(|text| text == atom),
            "{atom:?} must render without a mid-token split: {strings:?}"
        );
    }
    assert!(
        !strings.iter().any(|text| text == "new_signu"),
        "headers may wrap after '_' but never at an arbitrary character: {strings:?}"
    );
}

#[test]
fn csv_wrapping_preserves_interior_whitespace_runs() {
    // Whitespace is a legal wrap opportunity, not permission to normalize
    // quoted cell data. When both atoms fit on one line, the exact separator
    // from the source must survive.
    let csv = "id,notes\n1,\"alpha  beta   gamma\"\n";
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);
    assert!(
        strings.iter().any(|text| text == "alpha  beta   gamma"),
        "interior whitespace must be preserved exactly: {strings:?}"
    );
}

#[test]
fn csv_impossibly_dense_columns_fail_closed_without_overlapping_ellipses() {
    // At the 4pt floor, a table must not scale columns below both their
    // padding and a visible atom/ellipsis marker. Such geometry would place
    // text outside its cell, so fail closed with a typed resource limit.
    let headers: Vec<String> = (0..100).map(|i| format!("header_{i:03}")).collect();
    let values: Vec<String> = (0..100).map(|i| format!("value-{i:03}")).collect();
    let csv = format!("{}\n{}\n", headers.join(","), values.join(","));
    let result = std::panic::catch_unwind(|| csv_to_pdf(csv.as_bytes(), &DocOptions::default()));
    let error = result
        .expect("dense columns must not panic")
        .expect_err("geometry too dense for a visible marker");
    assert!(
        matches!(error, PdfCoreError::ResourceLimit(ref message) if message.contains("visible")),
        "{error:?}"
    );
}

#[test]
fn csv_oversized_atom_ellipsizes_only_after_reaching_font_floor() {
    // Rule change: table cells no longer character-wrap an indivisible token
    // to preserve every byte. The table first shrinks to the 4pt floor; an
    // atom that still cannot fit is visibly clipped with an ellipsis.
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
    assert!(
        x_total < 600,
        "an atom wider than the page at 4pt must be clipped: {strings:?}"
    );
    assert!(
        strings
            .iter()
            .any(|text| text.starts_with('x') && text.ends_with("...")),
        "clipping must be explicit, never a silent split: {strings:?}"
    );
    assert!(
        tf_sizes(&content)
            .iter()
            .all(|size| (*size - 4.0).abs() < 0.01),
        "oversized atom must reach the 4pt floor before clipping: {:?}",
        tf_sizes(&content)
    );
}

#[test]
fn csv_sixty_line_cell_paginates_without_truncation() {
    // Regression: shared CSV/Markdown table layout capped a cell at eight
    // lines and appended an ellipsis. A row must instead continue below its
    // first page fragment with every cell kept at its original column x.
    let cell = (1..=60)
        .map(|n| format!("cell-line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let csv = format!("Name,Description\nitem,\"{cell}\"\n");
    let opts = DocOptions {
        page_size: PageSize::LetterLandscape,
        ..DocOptions::default()
    };
    let bytes = csv_to_pdf(csv.as_bytes(), &opts).unwrap();
    let doc = load(&bytes);
    assert!(
        doc.get_pages().len() >= 2,
        "60 lines must split one row across pages"
    );
    let extracted = extract_text(&bytes).expect("extract_text");
    for n in 1..=60 {
        assert!(
            extracted.contains(&format!("cell-line-{n:02}")),
            "cell line {n} missing: {extracted:?}"
        );
    }
    assert!(
        !extracted.contains("..."),
        "long cell must not acquire an ellipsis: {extracted:?}"
    );
    let line_xs: Vec<_> = (0..doc.get_pages().len())
        .flat_map(|page| text_ops(&page_content_str(&doc, page)))
        .filter(|(_, _, text)| text.starts_with("cell-line-"))
        .map(|(x, _, _)| x)
        .collect();
    assert_eq!(line_xs.len(), 60, "all lines must have a text operation");
    assert!(
        line_xs.iter().all(|x| (*x - line_xs[0]).abs() < 0.01),
        "cell continuation must stay in its column: {line_xs:?}"
    );
    for page in 0..doc.get_pages().len() {
        assert!(
            page_content_str(&doc, page).contains(" re\n"),
            "row fragment needs cell borders on page {page}"
        );
    }
}

#[test]
fn csv_row_wrapping_at_page_boundary_stays_atomic() {
    // Regression: a logical row whose wrapped cell lines crossed the page
    // boundary used to split mid-row — page N ended with the row's first
    // lines and page N+1 resumed with orphaned continuation fragments under
    // the repeated header. A row that fits on a full page must instead move
    // whole to the next page; only rows taller than a page may split.
    let mut csv = String::from("OrderId,Notes\n");
    for i in 0..30 {
        csv.push_str(&format!(
            "ROW-{i:03},\"note{i:03}-line1\nnote{i:03}-line2\nnote{i:03}-line3\"\n"
        ));
    }
    let bytes = csv_to_pdf(csv.as_bytes(), &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let n_pages = doc.get_pages().len();
    assert!(n_pages >= 2, "30 three-line rows must paginate");

    let mut line_occurrences = 0usize;
    for page in 0..n_pages {
        let ops = text_ops(&page_content_str(&doc, page));
        let page_strings: Vec<&str> = ops.iter().map(|(_, _, text)| text.as_str()).collect();
        // Atomicity: any fragment of a row on this page implies the whole
        // row (its id cell and all three note lines) is on this page.
        for i in 0..30 {
            let fragments = ["line1", "line2", "line3"]
                .iter()
                .filter(|l| page_strings.contains(&format!("note{i:03}-{l}").as_str()))
                .count();
            if fragments == 0 {
                continue;
            }
            line_occurrences += fragments;
            assert_eq!(
                fragments, 3,
                "row {i} split across a page boundary: page {page} holds only \
                 {fragments} of its 3 note lines"
            );
            assert!(
                page_strings.contains(&format!("ROW-{i:03}").as_str()),
                "row {i} note lines on page {page} lack their OrderId cell"
            );
        }
        // The first data baseline under the (repeated) header is a row
        // START — an OrderId cell — never a continuation fragment.
        let data_top = ops
            .iter()
            .filter(|(_, _, text)| text.starts_with("ROW-") || text.starts_with("note"))
            .map(|(_, y, _)| *y)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            ops.iter()
                .any(|(_, y, text)| (*y - data_top).abs() < 0.01 && text.starts_with("ROW-")),
            "page {page}: topmost data line must be a row start, not a continuation"
        );
    }
    assert_eq!(line_occurrences, 90, "every note line renders exactly once");
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
    let opts = DocOptions::default(); // 14 CSS px = 10.5pt
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
    // Chromium user-agent scales: 2em / 1.5em / 1.17em.
    assert!((sizes[0] - 21.0).abs() < 0.01, "{sizes:?}");
    assert!((sizes[1] - 15.75).abs() < 0.01, "{sizes:?}");
    assert!((sizes[2] - 12.285).abs() < 0.01, "{sizes:?}");
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
fn markdown_hard_breaks_force_lines_while_soft_breaks_reflow() {
    let hard = markdown_to_pdf("line1  \nline2", &DocOptions::default()).unwrap();
    let hard_doc = load(&hard);
    let hard_ops = text_ops(&page_content_str(&hard_doc, 0));
    let hard_lines: Vec<_> = hard_ops
        .iter()
        .filter(|(_, _, text)| text == "line1" || text == "line2")
        .collect();
    assert_eq!(hard_lines.len(), 2, "hard break lines: {hard_ops:?}");
    assert!(
        hard_lines[1].1 < hard_lines[0].1,
        "hard break must use descending baselines: {hard_lines:?}"
    );

    let soft = markdown_to_pdf("line1\nline2", &DocOptions::default()).unwrap();
    let soft_doc = load(&soft);
    let soft_ops = text_ops(&page_content_str(&soft_doc, 0));
    assert!(
        soft_ops.iter().any(|(_, _, text)| text == "line1 line2"),
        "soft break must reflow to one space: {soft_ops:?}"
    );
}

#[test]
fn markdown_paginates_and_replaces_images() {
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
    assert!(all.contains("[Image omitted for secure conversion]"));
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
    // Per-depth indent: 18pt from the measured 51.75pt content inset.
    assert!((alpha.0 - 69.75).abs() < 0.5, "depth-1 x: {}", alpha.0);
    assert!((beta.0 - 87.75).abs() < 0.5, "depth-2 x: {}", beta.0);
    assert!((gamma.0 - 105.75).abs() < 0.5, "depth-3 x: {}", gamma.0);
    assert!((delta.0 - 87.75).abs() < 0.5, "depth-2 x: {}", delta.0);
    assert!((epsilon.0 - 69.75).abs() < 0.5, "depth-1 x: {}", epsilon.0);

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
fn markdown_html_blocks_render_literal_source() {
    let md = "Intro paragraph.\n\n<div class=\"box\">\nVisible secret &amp; friends\n</div>\n\n\
              Mid <b>inline kept</b> text.\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);

    assert!(
        strings
            .iter()
            .any(|s| s.contains("Visible secret &amp; friends")),
        "HTML block source text must render literally: {strings:?}"
    );
    assert!(
        strings.iter().any(|s| s.contains("inline kept")),
        "inline HTML inner text must survive: {strings:?}"
    );
    assert!(
        strings.iter().any(|s| s.contains("<div class=\"box\">")),
        "block tag source must survive: {strings:?}"
    );
    assert!(
        strings.iter().any(|s| s.contains("<b>")) && strings.iter().any(|s| s.contains("</b>")),
        "inline tag source must survive: {strings:?}"
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

#[test]
fn markdown_table_uses_the_same_token_safe_layout_as_csv() {
    // The CSV and GFM table renderers share layout helpers. Joined cell atoms
    // must remain indivisible in both formats even when many columns force a
    // shared font-size reduction.
    let md = "\
| week_start | order_id | mrr_usd | active_users | new_signups | activation_pct | retention_d7_pct | expansion_usd |\n\
| --- | --- | --- | --- | --- | --- | --- | --- |\n\
| 2026-04-01 | SO-30412 | 216797.3 | 18883 | 480 | 37.9 | 27.5 | 8660.04 |\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let strings = all_show_strings(&doc);
    for atom in [
        "week_start",
        "2026-04-01",
        "SO-30412",
        "216797.3",
        "8660.04",
    ] {
        assert!(
            strings.iter().any(|text| text == atom),
            "{atom:?} must render without a mid-token split: {strings:?}"
        );
    }
}

#[test]
fn winansi_text_round_trips_through_standard_font() {
    let source = "café ü € – “quoted” \"straight\"";
    let bytes = markdown_to_pdf(source, &DocOptions::default()).unwrap();
    let extracted = extract_text(&bytes).expect("extract WinAnsi Markdown");

    for expected in ["café", "ü", "€", "–", "“quoted”", "\"straight\""] {
        assert!(
            extracted.contains(expected),
            "missing {expected:?}: {extracted:?}"
        );
    }
}

#[test]
fn text_to_pdf_preserves_latin1() {
    let bytes = text_to_pdf("café", &DocOptions::default()).unwrap();
    let extracted = extract_text(&bytes).expect("extract Latin-1 text PDF");

    assert!(extracted.contains("café"), "got {extracted:?}");
}

#[test]
fn javascript_link_renders_without_annotation() {
    let bytes = markdown_to_pdf("[visible](javascript:alert(1))", &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let page_id = page_ids(&doc)[0];
    let extracted = extract_text(&bytes).expect("extract unsafe-link PDF");

    assert!(
        extracted.contains("visible"),
        "link text missing: {extracted:?}"
    );
    assert!(
        doc.get_page_annotations(page_id).unwrap().is_empty(),
        "unsafe URI must not create an annotation"
    );
}

#[test]
fn multiline_link_annotation_rects_stay_inside_page() {
    let label = "linked words ".repeat(30);
    let markdown = format!("[{label}](https://example.com/wrapped)");
    let opts = DocOptions {
        margin_pt: 200.0,
        ..DocOptions::default()
    };
    let bytes = markdown_to_pdf(&markdown, &opts).unwrap();
    let doc = load(&bytes);
    let annotations: Vec<_> = page_ids(&doc)
        .into_iter()
        .flat_map(|page_id| doc.get_page_annotations(page_id).unwrap())
        .collect();

    assert!(annotations.len() > 1, "expected wrapped link fragments");
    let (page_w, page_h) = opts.page_size.dimensions_pt();
    for annotation in annotations {
        let rect = annotation.get(b"Rect").unwrap().as_array().unwrap();
        let values: Vec<f64> = rect
            .iter()
            .map(|value| f64::from(value.as_float().unwrap()))
            .collect();
        assert!(
            values[0] >= 0.0
                && values[1] >= 0.0
                && values[0] < values[2]
                && values[1] < values[3]
                && values[2] <= page_w
                && values[3] <= page_h,
            "invalid annotation rect: {values:?}"
        );
    }
}

#[test]
fn bare_urls_emit_link_annotations() {
    let bytes = markdown_to_pdf(
        "Visit www.example.com or https://example.org/path.",
        &DocOptions::default(),
    )
    .unwrap();
    let doc = load(&bytes);
    let annotations = doc.get_page_annotations(page_ids(&doc)[0]).unwrap();

    assert_eq!(annotations.len(), 2);
}

#[test]
fn task_list_markers_use_rectangles_at_bullet_indent() {
    let bytes = markdown_to_pdf("- [x] done\n- [ ] open", &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let content = page_content_str(&doc, 0);
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let marker_rects = tokens
        .windows(5)
        .filter(|window| {
            window[4] == "re"
                && window[0]
                    .parse::<f64>()
                    .is_ok_and(|x| (54.0..72.0).contains(&x))
        })
        .count();

    assert_eq!(marker_rects, 2, "task marker rects missing: {content}");
}

#[test]
fn markdown_footnote_definitions_render_backreference_arrow() {
    register_font(include_bytes!("../wasm/pkg/fonts/NotoEmoji-Variable.ttf"))
        .expect("register font covering backreference arrow");
    let bytes = markdown_to_pdf(
        "Reference[^note].\n\n[^note]: Definition text.",
        &DocOptions::default(),
    )
    .expect("render footnote");
    let extracted = extract_text(&bytes).expect("extract footnote");

    assert!(
        extracted.contains("1. Definition text. ↩"),
        "footnote backreference missing: {extracted:?}"
    );
}

#[test]
fn markdown_heading_group_keeps_with_next_across_page_boundary() {
    // Regression: headings drew with no look-ahead, so a chained heading
    // group (H2 immediately followed by H3) could strand at the bottom of a
    // page — the group at the page foot with its first body line orphaned on
    // the next page, or the H3 split from its H2. The whole contiguous
    // heading group must instead move together with at least one body line.
    // Sweep the filler count so the group crosses the boundary at several
    // offsets; the invariant must hold for every one of them.
    for filler in 18..=28 {
        let mut md = String::new();
        for i in 0..filler {
            md.push_str(&format!("Filler paragraph number {i}.\n\n"));
        }
        md.push_str("## Keep Heading\n\n### Keep Subheading\n\n- keep item one\n- keep item two\n");
        let bytes = markdown_to_pdf(&md, &DocOptions::default()).unwrap();
        let doc = load(&bytes);
        let n_pages = doc.get_pages().len();
        let page_of = |needle: &str| -> usize {
            (0..n_pages)
                .find(|&i| {
                    show_strings(&page_content_str(&doc, i))
                        .iter()
                        .any(|s| s.contains(needle))
                })
                .unwrap_or_else(|| panic!("{needle:?} missing (filler={filler})"))
        };
        let h2_page = page_of("Keep Heading");
        let h3_page = page_of("Keep Subheading");
        let item_page = page_of("keep item one");
        assert_eq!(
            h3_page, h2_page,
            "H3 must stay on the H2's page (filler={filler})"
        );
        assert_eq!(
            item_page, h2_page,
            "first body line must stay with its heading group (filler={filler})"
        );
    }
}

#[test]
fn markdown_blockquote_paragraphs_indent_relative_to_body() {
    // Regression: blockquotes were flattened to full-width body paragraphs.
    // Quoted text must indent 18pt per quote level and return to the body
    // margin after the quote closes.
    let md = "Plain body paragraph.\n\n> Quoted body paragraph.\n\n\
              > > Deeply quoted paragraph.\n\nAfter body paragraph.\n";
    let bytes = markdown_to_pdf(md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let ops = text_ops(&page_content_str(&doc, 0));
    let x_of = |needle: &str| -> f64 {
        ops.iter()
            .find(|(_, _, text)| text.contains(needle))
            .map(|(x, _, _)| *x)
            .unwrap_or_else(|| panic!("{needle:?} missing as a text op: {ops:?}"))
    };
    let plain_x = x_of("Plain body paragraph.");
    let quote_x = x_of("Quoted body paragraph.");
    let deep_x = x_of("Deeply quoted paragraph.");
    let after_x = x_of("After body paragraph.");
    assert!(
        (quote_x - plain_x - 18.0).abs() < 0.5,
        "blockquote must indent 18pt from body: body x={plain_x}, quote x={quote_x}"
    );
    assert!(
        (deep_x - plain_x - 36.0).abs() < 0.5,
        "nested blockquote must indent 36pt from body: body x={plain_x}, deep x={deep_x}"
    );
    assert!(
        (after_x - plain_x).abs() < 0.01,
        "indent must reset after the quote closes: body x={plain_x}, after x={after_x}"
    );
}

#[test]
fn hostile_markdown_blockquote_nesting_returns_typed_limit_without_panic() {
    let markdown = format!("{}deeply quoted\n", "> ".repeat(500));
    let result = std::panic::catch_unwind(|| markdown_to_pdf(&markdown, &DocOptions::default()));
    let error = result.expect("must not panic").expect_err("nesting limit");
    assert!(matches!(error, PdfCoreError::InvalidInput(ref message) if message.contains("limit")));
}

#[test]
fn markdown_code_block_at_page_boundary_stays_atomic() {
    // Regression: a fenced block near the page bottom split mid-block even
    // when the whole block fit a fresh page. Same policy as CSV row
    // atomicity: a block that fits a full page moves whole to the next page;
    // only blocks taller than a full page may split.
    for filler in 18..=26 {
        let mut md = String::new();
        for i in 0..filler {
            md.push_str(&format!("Filler paragraph number {i}.\n\n"));
        }
        md.push_str("```\n");
        for n in 1..=8 {
            md.push_str(&format!("atomfence-{n}\n"));
        }
        md.push_str("```\n");
        let bytes = markdown_to_pdf(&md, &DocOptions::default()).unwrap();
        let doc = load(&bytes);
        let n_pages = doc.get_pages().len();
        let line_pages: Vec<usize> = (1..=8)
            .map(|n| {
                let needle = format!("atomfence-{n}");
                (0..n_pages)
                    .find(|&i| {
                        show_strings(&page_content_str(&doc, i))
                            .iter()
                            .any(|s| s.contains(&needle))
                    })
                    .unwrap_or_else(|| panic!("{needle:?} missing (filler={filler})"))
            })
            .collect();
        assert!(
            line_pages.iter().all(|&p| p == line_pages[0]),
            "code block that fits a fresh page must not split \
             (filler={filler}): line pages {line_pages:?}"
        );
    }

    // A block taller than a full page must still split rather than loop or
    // drop lines: every line renders exactly once, across at least 2 pages.
    let mut md = String::from("```\n");
    for n in 1..=60 {
        md.push_str(&format!("tallfence-{n:02}\n"));
    }
    md.push_str("```\n");
    let bytes = markdown_to_pdf(&md, &DocOptions::default()).unwrap();
    let doc = load(&bytes);
    let n_pages = doc.get_pages().len();
    assert!(
        n_pages >= 2,
        "60 code lines exceed one page and must paginate"
    );
    let all = all_show_strings(&doc);
    for n in 1..=60 {
        let needle = format!("tallfence-{n:02}");
        let count = all.iter().filter(|s| s.contains(&needle)).count();
        assert_eq!(count, 1, "{needle} must render exactly once");
    }
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
