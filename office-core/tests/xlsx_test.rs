//! XLSX conversion integration tests (agent task 6).

use office_core::layout::{EncodedImage, Item, Page};
use office_core::testkit::zip_package;
use office_core::xlsx::{XlsxDoc, numfmt, render_xlsx};
use office_core::{ConvertOptions, OfficeFormat, convert, profile, profile_with_options};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

fn unicode_opts() -> ConvertOptions {
    ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    }
}

fn wrapped_unicode_xlsx(source: &str) -> Vec<u8> {
    let sheet = format!(
        r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="1" width="8"/></cols><sheetData><row r="1"><c r="A1" s="1" t="inlineStr"><is><t>{source}</t></is></c></row></sheetData></worksheet>"#
    );
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><name val="Calibri"/></font><font><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="2"><xf fontId="0"/><xf fontId="1"><alignment wrapText="1"/></xf></cellXfs></styleSheet>"#;
    minimal_xlsx(&sheet, Some(styles))
}

fn unicode_header_footer_xlsx() -> Vec<u8> {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><headerFooter><oddHeader>&amp;Lمرحبا</oddHeader><oddFooter>&amp;Rनमस्ते</oddFooter></headerFooter></worksheet>"#;
    minimal_xlsx(sheet, None)
}

fn page_text(page: &Page) -> String {
    let mut s = String::new();
    for pos in &page.items {
        if let Item::Glyphs(run) = &pos.item {
            for g in &run.glyphs {
                s.push(g.ch);
            }
        }
    }
    s
}

fn two_sheet_xlsx_with_corrupt_second() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/><sheet name="Sheet2" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#;
    let sheet1 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>ok</t></is></c></row></sheetData></worksheet>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
        ("xl/worksheets/sheet2.xml", b"<not-xml"),
    ])
}

fn minimal_xlsx(sheet: &str, styles: Option<&str>) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let mut parts = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ];
    if let Some(styles) = styles {
        parts.push(("xl/styles.xml", styles.as_bytes()));
    }
    zip_package(&parts)
}

fn xlsx_with_drawing(drawing: &str, drawing_rels: &str) -> Vec<u8> {
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    xlsx_with_drawing_image(drawing, drawing_rels, Some(&image))
}

fn xlsx_with_drawing_image(drawing: &str, drawing_rels: &str, image: Option<&[u8]>) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><drawing r:id="rIdDraw"/></worksheet>"#;
    let sheet_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDraw" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#;
    let mut parts = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels.as_bytes()),
        ("xl/drawings/drawing1.xml", drawing.as_bytes()),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            drawing_rels.as_bytes(),
        ),
    ];
    if let Some(image) = image {
        parts.push(("xl/media/image1.png", image));
    }
    zip_package(&parts)
}

fn assert_xlsx_image_reason(bytes: &[u8], expected_reason: &str) {
    let prof = profile(bytes, OfficeFormat::Xlsx).expect("profile image fixture");
    assert!(!prof.supported, "profile accepted {expected_reason}");
    assert!(
        prof.reasons
            .iter()
            .any(|reason| { serde_json::to_value(reason.code).unwrap() == expected_reason }),
        "missing {expected_reason}: {:?}",
        prof.reasons
    );
    let error = convert(bytes, OfficeFormat::Xlsx, &opts()).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = error else {
        panic!("conversion did not return typed unsupported: {error:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);

    let doc = XlsxDoc::open(bytes, &opts()).expect("open image fixture");
    let direct = render_xlsx(&doc).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = direct else {
        panic!("direct XLSX path did not fail typed: {direct:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);
}

fn xlsx_with_cached_chart(chart_xml: &str) -> Vec<u8> {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx="3657600" cy="1828800"/><xdr:graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rIdChart"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdChart" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#;
    let base = xlsx_with_drawing(drawing, rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .unwrap();
    let xml = String::from_utf8(content_types.1.clone())
        .unwrap()
        .replace("</Types>", r#"<Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/></Types>"#);
    content_types.1 = xml.into_bytes();
    entries.push((
        "xl/charts/chart1.xml".to_string(),
        chart_xml.as_bytes().to_vec(),
    ));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn xlsx_with_theme(sheet: &str, styles: &str, theme: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
        ("xl/theme/theme1.xml", theme.as_bytes()),
    ])
}

fn xlsx_with_defined_names(sheet: &str, defined_names: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = format!(
        r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets><definedNames>{defined_names}</definedNames></workbook>"#
    );
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

#[test]
fn converts_every_mock_xlsx_to_valid_pdf() {
    for sub in ["xlsx", "xlsm"] {
        let dir = std::path::Path::new("../mock-files").join(sub);
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|error| {
            panic!("required fixture directory {}: {error}", dir.display())
        });
        let mut checked = 0;
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("read fixture under {}: {error}", dir.display()))
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some(sub) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let out = convert(&bytes, OfficeFormat::Xlsx, &opts())
                .unwrap_or_else(|e| panic!("convert {}: {e}", path.display()));
            assert!(out.pdf.starts_with(b"%PDF-"));
            assert!(out.page_count >= 1, "{} had 0 pages", path.display());
            checked += 1;
        }
        assert!(checked > 0, "no required {sub} fixtures found");
    }
}

#[test]
fn sheet_cells_render_with_values() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let doc = XlsxDoc::open(&bytes, &opts()).expect("open");
    let (laid, _) = render_xlsx(&doc).expect("render");
    assert!(!laid.pages.is_empty());
    let text: String = laid.pages.iter().map(page_text).collect();
    assert!(text.contains("Column A"), "header missing: {text:?}");
    assert!(text.contains("one"), "shared string missing: {text:?}");
    // Booleans render as TRUE/FALSE.
    assert!(text.contains("TRUE") || text.contains("FALSE"));
}

#[test]
fn xlsx_conversion_is_deterministic() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let a = convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "xlsx output must be deterministic");
}

#[test]
fn xlsx_profile_reports_sheets() {
    let bytes = std::fs::read("../mock-files/xlsx/file1.xlsx").unwrap();
    let prof = profile(&bytes, OfficeFormat::Xlsx).expect("profile");
    assert_eq!(prof.format, "xlsx");
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    assert!(prof.stats.sheets >= 1);
}

/// Router contract for XLSX/XLSM: `profile().supported == convert().is_ok()` on
/// every real mock file. `convert_xlsx` profiles first and fails closed, so the
/// unsupported→unsupported direction is structural; the risk this guards is a
/// profile *false positive* — a file profile passes that then fails in
/// `XlsxDoc::open`/render/emit. DocKosha's engine-first router depends on this.
#[test]
fn xlsx_profile_agrees_with_convert_on_mock_corpus() {
    let mut checked = 0;
    for sub in ["xlsx", "xlsm"] {
        let dir = std::path::Path::new("../mock-files").join(sub);
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|error| {
            panic!("required fixture directory {}: {error}", dir.display())
        });
        let mut checked_for_format = 0;
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("read fixture under {}: {error}", dir.display()))
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some(sub) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let prof = profile(&bytes, OfficeFormat::Xlsx).expect("profile");
            let conv = convert(&bytes, OfficeFormat::Xlsx, &opts());
            assert_eq!(
                prof.supported,
                conv.is_ok(),
                "profile/convert verdict mismatch for {}: reasons={:?} err={:?}",
                path.display(),
                prof.reasons,
                conv.err(),
            );
            checked += 1;
            checked_for_format += 1;
        }
        assert!(checked_for_format > 0, "no required {sub} fixtures found");
    }
    assert!(checked > 0, "no mock xlsx/xlsm files found");
}

#[test]
fn wrapped_unicode_cells_measure_and_paint_with_cluster_fallbacks() {
    for source in [
        "مرحبا بالعالم مرحبا بالعالم",
        "नमस्ते दुनिया नमस्ते दुनिया",
        "สวัสดีชาวโลก สวัสดีชาวโลก",
    ] {
        let bytes = wrapped_unicode_xlsx(source);
        let options = unicode_opts();
        let converted = convert(&bytes, OfficeFormat::Xlsx, &options);
        assert!(
            converted.is_ok(),
            "failed to convert {source:?}: {converted:?}"
        );

        let doc = XlsxDoc::open(&bytes, &options).expect("open wrapped Unicode XLSX");
        let (laid, _) = render_xlsx(&doc).expect("render wrapped Unicode XLSX");
        let extracted: String = laid
            .pages
            .iter()
            .map(page_text)
            .collect::<String>()
            .chars()
            .filter(|character| *character != '\0')
            .collect();
        assert!(
            extracted.contains(source),
            "logical source missing for {source:?}: {extracted:?}"
        );
    }
}

#[test]
fn unicode_xlsx_headers_and_footers_use_cluster_fallbacks() {
    let bytes = unicode_header_footer_xlsx();
    let options = unicode_opts();
    let converted = convert(&bytes, OfficeFormat::Xlsx, &options);
    assert!(converted.is_ok(), "header/footer conversion: {converted:?}");

    let doc = XlsxDoc::open(&bytes, &options).expect("open Unicode header/footer XLSX");
    let (laid, _) = render_xlsx(&doc).expect("render Unicode header/footer XLSX");
    let extracted: String = laid.pages.iter().map(page_text).collect();
    assert!(
        extracted.contains("مرحبا"),
        "missing Arabic header: {extracted:?}"
    );
    assert!(
        extracted.contains("नमस्ते"),
        "missing Devanagari footer: {extracted:?}"
    );
}

#[test]
fn xlsx_profile_supported_implies_convert_succeeds_for_unicode_fixtures() {
    let mut fixtures = vec![unicode_header_footer_xlsx()];
    fixtures.extend(
        [
            "مرحبا بالعالم مرحبا بالعالم",
            "नमस्ते दुनिया नमस्ते दुनिया",
            "สวัสดีชาวโลก สวัสดีชาวโลก",
        ]
        .map(wrapped_unicode_xlsx),
    );
    let options = unicode_opts();
    for bytes in fixtures {
        let prof = profile(&bytes, OfficeFormat::Xlsx).expect("profile Unicode XLSX");
        let converted = convert(&bytes, OfficeFormat::Xlsx, &options);
        assert!(
            !prof.supported || converted.is_ok(),
            "profile accepted bytes conversion refused: reasons={:?} error={:?}",
            prof.reasons,
            converted.err()
        );
    }
}

#[test]
fn corrupt_sheet_profile_and_convert_name_sheet() {
    let bytes = two_sheet_xlsx_with_corrupt_second();
    for error in [
        profile(&bytes, OfficeFormat::Xlsx).unwrap_err(),
        convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            office_core::OfficeError::Invalid(detail) if detail.contains("Sheet2")
        ));
    }
}

#[test]
fn merged_cells_render_one_text_region_without_interior_gridline() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><printOptions gridLines="1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>MERGED</t></is></c><c r="B1" t="inlineStr"><is><t>PHANTOM</t></is></c></row></sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    assert_eq!(text.matches("MERGED").count(), 1);
    assert!(!text.contains("PHANTOM"));
    let line_count = laid.pages[0]
        .items
        .iter()
        .filter(|item| matches!(item.item, Item::Line { .. }))
        .count();
    assert_eq!(
        line_count, 4,
        "merged region must have only outer gridlines"
    );
}

#[test]
fn wrapped_text_grows_automatic_row_and_styles_render() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="1" width="8"/></cols><sheetData><row r="1"><c r="A1" s="1" t="inlineStr"><is><t>one two three four five six seven eight</t></is></c></row></sheetData></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/></font><font><i/><sz val="11"/><color rgb="FF204080"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FFFFDD80"/></patternFill></fill></fills><borders count="2"><border/><border><left style="thin"><color rgb="FF102030"/></left><right style="medium"><color rgb="FF102030"/></right><top style="thick"><color rgb="FF102030"/></top><bottom style="thin"><color rgb="FF102030"/></bottom></border></borders><cellXfs count="2"><xf fontId="0" fillId="0" borderId="0"/><xf fontId="1" fillId="1" borderId="1"><alignment wrapText="1"/></xf></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(sheet, Some(styles));
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let style = doc.styles.cell(1);
    assert!(style.italic && style.wrap && style.fill.is_some());
    assert!(style.borders.top.is_some() && style.borders.right.is_some());
    let (laid, _) = render_xlsx(&doc).unwrap();
    let bottom = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match positioned.item {
            Item::Line { dy_pt: 0.0, .. } => Some(positioned.y_pt),
            _ => None,
        })
        .fold(0.0, f64::max);
    assert!(
        bottom > 51.0,
        "wrapped auto row did not grow: bottom={bottom}"
    );
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Rect { fill: Some(_), .. }))
    );
}

#[test]
fn unsupported_conditional_formatting_profile_and_convert_agree_on_typed_miss() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="iconSet"><iconSet iconSet="3TrafficLights1"/></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| { reason.code == office_core::UnsupportedCode::ConditionalFormatting })
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Xlsx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: office_core::UnsupportedCode::ConditionalFormatting,
            ..
        })
    ));
}

#[test]
fn cell_is_conditional_formatting_is_rendered_not_rejected() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="cellIs" operator="greaterThan"><formula>0</formula></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn one_cell_anchored_picture_with_source_crop_renders() {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:oneCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>12700</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>25400</xdr:rowOff></xdr:from><xdr:ext cx="914400" cy="914400"/><xdr:pic><xdr:nvPicPr/><xdr:blipFill><a:blip r:embed="rIdImg"/><a:srcRect l="50000"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    let bytes = xlsx_with_drawing(drawing, rels);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert_eq!(laid.images.len(), 1);
    let original =
        image::load_from_memory(&std::fs::read("../mock-files/png/file1.png").unwrap()).unwrap();
    let cropped_width = match &laid.images[0] {
        EncodedImage::Jpeg { width, .. }
        | EncodedImage::Rgb8 { width, .. }
        | EncodedImage::Rgba8 { width, .. } => *width,
    };
    assert_eq!(cropped_width, original.width() / 2);
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::Image { .. }))
    );
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn visible_xlsx_pictures_fail_closed_when_relationship_decode_or_crop_is_invalid() {
    let drawing = |crop: &str| {
        format!(
            r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx="914400" cy="914400"/><xdr:pic><xdr:nvPicPr/><xdr:blipFill><a:blip r:embed="rIdImg"/>{crop}<a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:pic><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#
        )
    };
    let image_rel = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;

    let missing_relationship = xlsx_with_drawing_image(
        &drawing(""),
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        None,
    );
    assert_xlsx_image_reason(&missing_relationship, "missing_relationship_target");

    let missing_part = xlsx_with_drawing_image(&drawing(""), image_rel, None);
    assert_xlsx_image_reason(&missing_part, "missing_relationship_target");

    let corrupt = xlsx_with_drawing_image(&drawing(""), image_rel, Some(b"not an image"));
    assert_xlsx_image_reason(&corrupt, "image_decode_failure");

    let disguised_tiff =
        xlsx_with_drawing_image(&drawing(""), image_rel, Some(b"II*\0unsupported tiff"));
    assert_xlsx_image_reason(&disguised_tiff, "unsupported_image_encoding");

    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let invalid_crop = xlsx_with_drawing_image(
        &drawing(r#"<a:srcRect l="60000" r="60000"/>"#),
        image_rel,
        Some(&image),
    );
    assert_xlsx_image_reason(&invalid_crop, "invalid_image_crop");
}

#[test]
fn anchored_ellipse_shape_renders_with_preset_geometry() {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><xdr:absoluteAnchor><xdr:pos x="12700" y="25400"/><xdr:ext cx="914400" cy="457200"/><xdr:sp><xdr:nvSpPr/><xdr:spPr><a:solidFill><a:srgbClr val="4472C4"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln><a:prstGeom prst="ellipse"/></xdr:spPr></xdr:sp><xdr:clientData/></xdr:absoluteAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = xlsx_with_drawing(drawing, rels);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::Rect { fill: Some(_), .. }))
    );
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn chart_drawing_stays_typed_and_profile_agrees_with_convert() {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx="914400" cy="914400"/><xdr:graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rIdChart"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = xlsx_with_drawing(drawing, rels);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == office_core::UnsupportedCode::Chart)
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Xlsx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: office_core::UnsupportedCode::Chart,
            ..
        })
    ));
}

#[test]
fn cached_xlsx_chart_call_site_renders_geometry_and_labels() {
    let chart = r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:areaChart><c:grouping val="standard"/><c:ser><c:idx val="0"/><c:tx><c:v>XLSX Area</c:v></c:tx><c:cat><c:strLit><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:formatCode>0</c:formatCode><c:pt idx="0"><c:v>13</c:v></c:pt><c:pt idx="1"><c:v>26</c:v></c:pt></c:numLit></c:val><c:dLbls><c:showVal val="1"/></c:dLbls></c:ser></c:areaChart><c:catAx><c:axId val="1"/></c:catAx><c:valAx><c:axId val="2"/></c:valAx></c:plotArea></c:chart></c:chartSpace>"#;
    let bytes = xlsx_with_cached_chart(chart);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(&item.item, Item::Path { paint, .. } if paint.fill.is_some()))
    );
    assert!(page_text(&laid.pages[0]).contains("13"));
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn color_scale_interpolates_cached_values() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>0</v></c><c r="B1"><v>50</v></c><c r="C1"><v>100</v></c></row></sheetData><conditionalFormatting sqref="A1:C1"><cfRule type="colorScale"><colorScale><cfvo type="min"/><cfvo type="percentile" val="50"/><cfvo type="max"/><color rgb="FFFF0000"/><color rgb="FFFFFF00"/><color rgb="FF00FF00"/></colorScale></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let fills: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|item| match &item.item {
            Item::Rect {
                fill: Some(fill), ..
            } => Some((fill.r, fill.g, fill.b)),
            _ => None,
        })
        .collect();
    assert!(fills.contains(&(255, 0, 0)), "fills={fills:?}");
    assert!(fills.contains(&(255, 255, 0)), "fills={fills:?}");
    assert!(fills.contains(&(0, 255, 0)), "fills={fills:?}");
    assert!(profile(&bytes, OfficeFormat::Xlsx).unwrap().supported);
}

#[test]
fn data_bar_width_tracks_cached_value() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="2" width="20"/></cols><sheetData><row r="1"><c r="A1"><v>25</v></c><c r="B1"><v>100</v></c></row></sheetData><conditionalFormatting sqref="A1:B1"><cfRule type="dataBar"><dataBar><cfvo type="num" val="0"/><cfvo type="max"/><color rgb="FF4472C4"/></dataBar></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let bars: Vec<f64> = laid.pages[0]
        .items
        .iter()
        .filter_map(|item| match &item.item {
            Item::Rect {
                w_pt,
                fill: Some(fill),
                ..
            } if (fill.r, fill.g, fill.b) == (68, 114, 196) => Some(*w_pt),
            _ => None,
        })
        .collect();
    assert_eq!(bars.len(), 2, "bars={bars:?}");
    assert!(bars[1] > bars[0] * 3.5, "bars={bars:?}");
    assert!(profile(&bytes, OfficeFormat::Xlsx).unwrap().supported);
}

#[test]
fn conditional_dxf_resolves_theme_and_indexed_colors() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>2</v></c><c r="B1"><v>2</v></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="cellIs" operator="greaterThan" dxfId="0"><formula>1</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B1"><cfRule type="cellIs" operator="greaterThan" dxfId="1"><formula>1</formula></cfRule></conditionalFormatting></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="2"><dxf><fill><patternFill patternType="solid"><fgColor theme="4"/></patternFill></fill></dxf><dxf><fill><patternFill patternType="solid"><fgColor indexed="3"/></patternFill></fill></dxf></dxfs></styleSheet>"#;
    let theme = r#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="test"><a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="111111"/></a:dk2><a:lt2><a:srgbClr val="EEEEEE"/></a:lt2><a:accent1><a:srgbClr val="123456"/></a:accent1><a:accent2><a:srgbClr val="654321"/></a:accent2></a:clrScheme><a:fontScheme name="test"><a:majorFont><a:latin typeface="Arial"/></a:majorFont><a:minorFont><a:latin typeface="Arial"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#;
    let bytes = xlsx_with_theme(sheet, styles, theme);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let fills: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|item| match &item.item {
            Item::Rect {
                fill: Some(fill), ..
            } => Some((fill.r, fill.g, fill.b)),
            _ => None,
        })
        .collect();
    assert!(fills.contains(&(0x12, 0x34, 0x56)), "fills={fills:?}");
    assert!(fills.contains(&(0, 255, 0)), "fills={fills:?}");
}

#[test]
fn print_title_columns_repeat_on_horizontal_pages() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="8" width="20"/></cols><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>TITLE</t></is></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c><c r="D1"><v>4</v></c><c r="E1"><v>5</v></c><c r="F1"><v>6</v></c><c r="G1"><v>7</v></c><c r="H1"><v>8</v></c></row></sheetData></worksheet>"#;
    let names =
        r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">'Sheet1'!$A:$A</definedName>"#;
    let bytes = xlsx_with_defined_names(sheet, names);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert!(laid.pages.len() >= 3, "pages={}", laid.pages.len());
    for (index, page) in laid.pages.iter().enumerate() {
        assert!(
            page_text(page).contains("TITLE"),
            "page {index} lacks title column"
        );
    }
}

#[test]
fn fit_to_height_one_scales_tall_sheet_to_one_page() {
    let rows: String = (1..=80)
        .map(|row| format!(r#"<row r="{row}" ht="20"><c r="A{row}"><v>{row}</v></c></row>"#))
        .collect();
    let sheet = format!(
        r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData><pageSetup fitToWidth="1" fitToHeight="1"/></worksheet>"#
    );
    let bytes = minimal_xlsx(&sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert_eq!(
        laid.pages.len(),
        1,
        "fitToHeight should constrain page count"
    );
}

#[test]
fn first_even_odd_headers_select_and_preserve_bold_italic() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row></sheetData><rowBreaks count="2" manualBreakCount="2"><brk id="1" man="1"/><brk id="2" man="1"/></rowBreaks><headerFooter differentFirst="1" differentOddEven="1"><oddHeader>&amp;L&amp;BOdd&amp;B</oddHeader><evenHeader>&amp;L&amp;IEven&amp;I</evenHeader><firstHeader>&amp;L&amp;BFirst&amp;B</firstHeader></headerFooter></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert_eq!(laid.pages.len(), 3);
    assert!(page_text(&laid.pages[0]).contains("First"));
    assert!(page_text(&laid.pages[1]).contains("Even"));
    assert!(page_text(&laid.pages[2]).contains("Odd"));
    let styled_face = |page: &Page, target: &str| {
        page.items.iter().find_map(|positioned| {
            let Item::Glyphs(run) = &positioned.item else {
                return None;
            };
            let text: String = run.glyphs.iter().map(|glyph| glyph.ch).collect();
            text.contains(target).then_some(run.face)
        })
    };
    let first_face = doc
        .fonts
        .face(styled_face(&laid.pages[0], "First").unwrap())
        .unwrap();
    let even_face = doc
        .fonts
        .face(styled_face(&laid.pages[1], "Even").unwrap())
        .unwrap();
    assert!(first_face.weight >= 700);
    assert!(even_face.italic);
}

#[test]
fn number_format_wave3_fraction_elapsed_optional_and_grouping_edges() {
    assert_eq!(numfmt::format_number(1.25, "# ?/?", false).text, "1 1/4");
    assert_eq!(
        numfmt::format_number(1.5, "[h]:mm:ss", false).text,
        "36:00:00"
    );
    assert_eq!(numfmt::format_number(1.2, "0.##", false).text, "1.2");
    assert_eq!(
        numfmt::format_number(-123456.0, "#,##0", false).text,
        "-123,456"
    );
    for id in [5, 6, 7, 8, 13, 41, 42, 43, 44] {
        assert!(numfmt::builtin_code(id).is_some(), "missing builtin {id}");
    }
}

#[test]
fn uncached_formula_fails_closed_with_stable_reason_and_cell_context() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>SECRET_FORMULA_TEXT()</f></c></row></sheetData></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(!prof.supported, "profile accepted an uncached formula");
    let reason = prof.reasons.first().expect("formula-cache reason");
    assert_eq!(
        serde_json::to_value(reason.code).unwrap(),
        "formula_cache_missing"
    );
    assert!(
        reason.detail.contains("sheet Sheet1"),
        "{:?}",
        reason.detail
    );
    assert!(reason.detail.contains("cell A1"), "{:?}", reason.detail);

    let error = convert(&bytes, OfficeFormat::Xlsx, &opts()).unwrap_err();
    let office_core::OfficeError::Unsupported { code, detail } = error else {
        panic!("expected typed unsupported error");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), "formula_cache_missing");
    assert!(detail.contains("sheet Sheet1"), "{detail:?}");
    assert!(detail.contains("cell A1"), "{detail:?}");
}

#[test]
fn present_empty_formula_cache_is_a_supported_blank_result() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>IF(TRUE,"",1)</f><v></v></c><c r="B1" t="str"><f>IF(TRUE,"",1)</f><v></v></c></row></sheetData></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);

    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn formula_type_annotations_do_not_mask_a_missing_cache() {
    for ty in ["str", "inlineStr", "s", "b", "e"] {
        let sheet = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="{ty}"><f t="shared" si="0"/></c></row></sheetData></worksheet>"#
        );
        let bytes = minimal_xlsx(&sheet, None);
        let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
        assert!(
            !prof.supported,
            "type {ty} masked the missing formula cache"
        );
        assert_eq!(
            serde_json::to_value(prof.reasons[0].code).unwrap(),
            "formula_cache_missing"
        );
        assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_err());
    }
}

#[test]
fn valid_cached_formula_result_types_render_without_formula_evaluation() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>40+2</f><v>42</v></c><c r="B1" t="str"><f>CONCAT("cache","d")</f><v>cached</v></c><c r="C1" t="inlineStr"><is><t>inline</t></is></c><c r="D1" t="b"><f>1=1</f><v>1</v></c><c r="E1" t="e"><f>1/0</f><v>#DIV/0!</v></c><c r="F1" s="1"><f>DATE(2024,1,1)</f><v>45292</v></c><c r="G1"><f>IF(TRUE,"",1)</f><v></v></c></row></sheetData></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy-mm-dd"/></numFmts><fonts count="1"><font/></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="2"><xf/><xf numFmtId="164" applyNumberFormat="1"/></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(sheet, Some(styles));

    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for cached in ["42", "cached", "inline", "TRUE", "#DIV/0!", "2024-01-01"] {
        assert!(text.contains(cached), "missing {cached:?}: {text:?}");
    }
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_ok());
}

#[test]
fn conditional_expression_supports_arithmetic_and_types_functions() {
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="1"><dxf><fill><patternFill patternType="solid"><fgColor rgb="FFFF0000"/></patternFill></fill></dxf></dxfs></styleSheet>"#;
    let supported = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>2</v></c><c r="B1"><v>1</v></c></row></sheetData><conditionalFormatting sqref="B1"><cfRule type="expression" dxfId="0"><formula>$A$1*2&gt;=4</formula></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(supported, Some(styles));
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|item| matches!(
        item.item,
        Item::Rect {
            fill: Some(fill),
            ..
        } if (fill.r, fill.g, fill.b) == (255, 0, 0)
    )));

    let unsupported = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>2</v></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="expression"><formula>SUM(A1)&gt;0</formula></cfRule></conditionalFormatting></worksheet>"#;
    let bytes = minimal_xlsx(unsupported, None);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == office_core::UnsupportedCode::ConditionalFormatting)
    );
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_err());
}

#[test]
fn visible_unimplemented_sheet_objects_fail_closed_with_specific_codes() {
    let cases = [
        (
            r#"<oleObjects><oleObject progId="Package" shapeId="1"/></oleObjects>"#,
            office_core::UnsupportedCode::OleObject,
        ),
        (
            r#"<hyperlinks><hyperlink ref="A1" location="Sheet1!A1"/></hyperlinks>"#,
            office_core::UnsupportedCode::Hyperlink,
        ),
        (
            r#"<sheetViews><sheetView workbookViewId="0" rightToLeft="1"/></sheetViews>"#,
            office_core::UnsupportedCode::RtlText,
        ),
    ];
    for (visible_object, code) in cases {
        let sheet = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>{visible_object}</worksheet>"#
        );
        let bytes = minimal_xlsx(&sheet, None);
        let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
        assert!(prof.reasons.iter().any(|reason| reason.code == code));
        assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_err());
    }
}

#[test]
fn drawing_marked_not_to_print_is_provably_ignored() {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:absoluteAnchor><xdr:pos x="0" y="0"/><xdr:ext cx="914400" cy="914400"/><xdr:pic><xdr:nvPicPr/><xdr:blipFill><a:blip r:embed="rIdImg"/></xdr:blipFill><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:pic><xdr:clientData fPrintsWithSheet="0"/></xdr:absoluteAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    let bytes = xlsx_with_drawing(drawing, rels);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    assert!(doc.images.is_empty());
    let (laid, _) = render_xlsx(&doc).unwrap();
    assert!(
        !laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::Image { .. }))
    );
}

#[test]
fn shared_and_array_formulas_render_only_cached_values() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f t="shared" si="0" ref="A1:A2">1+1</f><v>42</v></c><c r="B1"><f t="array" ref="B1:B2">ROW(B1:B2)</f><v>10</v></c></row><row r="2"><c r="A2"><f t="shared" si="0"/><v>43</v></c><c r="B2"><v>20</v></c></row></sheetData></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for cached in ["42", "43", "10", "20"] {
        assert!(text.contains(cached), "missing cached {cached}: {text:?}");
    }
    assert!(!text.contains("ROW"));
}

#[test]
fn xlsx_cluster_fallback_shapes_cjk_and_arabic_cells() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" s="1" t="inlineStr"><is><t>你好</t></is></c><c r="B1" s="1" t="inlineStr"><is><t>مرحبا</t></is></c></row></sheetData></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><name val="Calibri"/></font><font><name val="Arial"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="2"><xf fontId="0"/><xf fontId="1"/></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(sheet, Some(styles));
    let options = unicode_opts();
    let prof = profile_with_options(&bytes, OfficeFormat::Xlsx, &options).unwrap();
    assert!(prof.supported, "reasons: {:?}", prof.reasons);
    let doc = XlsxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for expected in ['你', '好', 'م', 'ر', 'ح', 'ب', 'ا'] {
        assert!(text.contains(expected), "missing {expected:?}: {text:?}");
    }
    assert!(convert(&bytes, OfficeFormat::Xlsx, &options).is_ok());
}

#[test]
fn xlsx_shaped_clusters_extract_complete_logical_source_once() {
    let source = "e\u{301} é لا שָׁ क्षि ก้ ⌚️ 👍🏽 👩‍💻 LTR שלום 123";
    let sheet = format!(
        r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" s="1" t="inlineStr"><is><t>{source}</t></is></c></row></sheetData></worksheet>"#
    );
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><name val="Calibri"/></font><font><name val="Arial"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="2"><xf fontId="0"/><xf fontId="1"/></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(&sheet, Some(styles));
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let doc = XlsxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let extracted: String = page_text(&laid.pages[0])
        .chars()
        .filter(|character| *character != '\0')
        .collect();
    assert_eq!(extracted, source);
}

#[test]
fn same_face_xlsx_cell_glyphs_stay_in_one_run() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>16</t></is></c></row></sheetData></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).expect("open XLSX");
    let (laid, _) = render_xlsx(&doc).expect("render XLSX");
    let glyph_runs = laid.pages[0]
        .items
        .iter()
        .filter(|item| matches!(item.item, Item::Glyphs(_)))
        .count();

    assert_eq!(
        glyph_runs, 1,
        "contiguous glyphs from one face must share a PDF run so extractors preserve multi-digit cells"
    );
}

#[test]
fn page_setup_paper_orientation_margins_and_gridline_default_apply() {
    let cases = [
        (1, "portrait", 612.0, 792.0),
        (5, "portrait", 612.0, 1008.0),
        (8, "portrait", 841.89, 1190.55),
        (9, "portrait", 595.28, 841.89),
        (1, "landscape", 792.0, 612.0),
    ];
    for (paper, orientation, expected_width, expected_height) in cases {
        let sheet = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><pageMargins left="1" right="0.5" top="1.25" bottom="0.75" header="0.3" footer="0.3"/><pageSetup paperSize="{paper}" orientation="{orientation}"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>X</t></is></c></row></sheetData></worksheet>"#
        );
        let bytes = minimal_xlsx(&sheet, None);
        let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
        let (laid, _) = render_xlsx(&doc).unwrap();
        let page = &laid.pages[0];
        assert!((page.width_pt - expected_width).abs() < 0.02);
        assert!((page.height_pt - expected_height).abs() < 0.02);
        let glyph = page
            .items
            .iter()
            .find(|item| matches!(item.item, Item::Glyphs(_)))
            .unwrap();
        assert!(
            glyph.x_pt >= 72.0,
            "left margin not applied: {}",
            glyph.x_pt
        );
        assert!(
            !page
                .items
                .iter()
                .any(|item| matches!(item.item, Item::Line { .. })),
            "print gridlines default off"
        );
    }
}

#[test]
fn print_area_excludes_cells_outside_named_range() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>KEEP</t></is></c><c r="B1" t="inlineStr"><is><t>DROP</t></is></c></row></sheetData></worksheet>"#;
    let names =
        r#"<definedName name="_xlnm.Print_Area" localSheetId="0">'Sheet1'!$A$1:$A$1</definedName>"#;
    let bytes = xlsx_with_defined_names(sheet, names);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("KEEP"));
    assert!(!text.contains("DROP"));
}

#[test]
fn header_footer_codes_expand_deterministically_and_unknown_code_drops_ampersand() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><headerFooter><oddHeader>&amp;L&amp;P/&amp;N &amp;D &amp;T &amp;F &amp;A &amp;Z</oddHeader></headerFooter></worksheet>"#;
    let bytes = minimal_xlsx(sheet, None);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for expected in ["1/1", "2000-01-01", "00:00", "workbook.xlsx", "Sheet1", "Z"] {
        assert!(text.contains(expected), "missing {expected:?}: {text:?}");
    }
    assert!(!text.contains("&Z"));
}

#[test]
fn two_cell_picture_anchor_uses_cell_geometry() {
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:pic><xdr:nvPicPr/><xdr:blipFill><a:blip r:embed="rIdImg"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:pic><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    let bytes = xlsx_with_drawing(drawing, rels);
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let (width, height) = laid.pages[0]
        .items
        .iter()
        .find_map(|item| match item.item {
            Item::Image { w_pt, h_pt, .. } => Some((w_pt, h_pt)),
            _ => None,
        })
        .unwrap();
    assert!((width - 48.0).abs() < 0.5, "width={width}");
    assert!((height - 15.0).abs() < 0.1, "height={height}");
}

#[test]
fn unsupported_pattern_fill_and_double_border_are_typed_not_silent() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" s="1"><v>1</v></c></row></sheetData></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="diagCross"><fgColor rgb="FFFF0000"/></patternFill></fill></fills><borders count="2"><border/><border><bottom style="double"><color rgb="FF000000"/></bottom></border></borders><cellXfs count="2"><xf/><xf fillId="1" borderId="1"/></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(sheet, Some(styles));
    let prof = profile(&bytes, OfficeFormat::Xlsx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.detail.contains("spreadsheet fill pattern"))
    );
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.detail.contains("double cell border"))
    );
    assert!(convert(&bytes, OfficeFormat::Xlsx, &opts()).is_err());
}

#[test]
fn cell_vertical_alignment_defaults_bottom_and_honors_top() {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1" ht="60"><c r="A1" t="inlineStr"><is><t>B</t></is></c><c r="B1" s="1" t="inlineStr"><is><t>T</t></is></c></row></sheetData></worksheet>"#;
    let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="2"><xf/><xf><alignment vertical="top"/></xf></cellXfs></styleSheet>"#;
    let bytes = minimal_xlsx(sheet, Some(styles));
    let doc = XlsxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_xlsx(&doc).unwrap();
    let baseline = |target: char| {
        laid.pages[0]
            .items
            .iter()
            .find_map(|item| match &item.item {
                Item::Glyphs(run) if run.glyphs.iter().any(|glyph| glyph.ch == target) => {
                    Some(item.y_pt)
                }
                _ => None,
            })
            .unwrap()
    };
    assert!(baseline('B') > baseline('T') + 25.0);
}
