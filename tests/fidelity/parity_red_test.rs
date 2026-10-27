//! Ignored parity fixtures that describe required behavior, not current behavior.

use image::DynamicImage;
use office_core::testkit::{DOC_NAMESPACES, minimal_docx, wrap_document, zip_package};
use office_core::{ConvertOptions, OfficeError, OfficeFormat, UnsupportedCode, convert, profile};
use pdf_core::extract::extract_text;
use pdf_core::meta::page_sizes;
use pdf_core::raster::{RenderOptions, render_page};

fn convert_fixture(bytes: &[u8], format: OfficeFormat) -> office_core::ConvertOutput {
    convert(bytes, format, &ConvertOptions::default()).expect("fixture must convert today")
}

fn raster(pdf: &[u8]) -> DynamicImage {
    let png = render_page(pdf, 0, &RenderOptions::default()).expect("rasterize first page");
    image::load_from_memory(&png).expect("decode first-page raster")
}

fn header_table_docx() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{ns}><w:body>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr>
</w:body></w:document>"#,
        ns = DOC_NAMESPACES,
    );
    let header = format!(
        r#"<?xml version="1.0"?><w:hdr{ns}><w:tbl>
<w:tblGrid><w:gridCol w:w="3000"/></w:tblGrid>
<w:tr><w:tc><w:p><w:r><w:t>HDRCELL</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl></w:hdr>"#,
        ns = DOC_NAMESPACES,
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
    ])
}

fn two_slide_pptx(slide2: &[u8]) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;
    let presentation = r#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst>
</p:presentation>"#;
    let presentation_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#;
    let slide1 = r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree/></p:cSld></p:sld>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
        (
            "ppt/_rels/presentation.xml.rels",
            presentation_rels.as_bytes(),
        ),
        ("ppt/slides/slide1.xml", slide1.as_bytes()),
        ("ppt/slides/slide2.xml", slide2),
    ])
}

fn two_sheet_xlsx(sheet2: &[u8]) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/><sheet name="Sheet2" sheetId="2" r:id="rId2"/></sheets>
</workbook>"#;
    let workbook_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
</Relationships>"#;
    let sheet1 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>ok</t></is></c></row></sheetData></worksheet>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
        ("xl/worksheets/sheet2.xml", sheet2),
    ])
}

fn transformed_shape_pptx(attributes: &str) -> Vec<u8> {
    let shape = format!(
        r#"<p:sp><p:spPr><a:xfrm {attributes}><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="914400"/></a:xfrm><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="D02020"/></a:gs><a:gs pos="100000"><a:srgbClr val="2040D0"/></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;
    let presentation = r#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>"#;
    let presentation_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#;
    let slide = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree>{shape}</p:spTree></p:cSld></p:sld>"#
    );
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
        (
            "ppt/_rels/presentation.xml.rels",
            presentation_rels.as_bytes(),
        ),
        ("ppt/slides/slide1.xml", slide.as_bytes()),
    ])
}

#[test]
fn docx_missing_glyph_must_render_or_return_error() {
    let input = minimal_docx(r#"<w:p><w:r><w:t>snowman ☃ survives</w:t></w:r></w:p>"#);
    let prof = profile(&input, OfficeFormat::Docx).expect("profile missing-glyph fixture");
    if prof.supported {
        let output = convert(&input, OfficeFormat::Docx, &ConvertOptions::default())
            .expect("a supported profile must convert the snowman");
        let extracted = extract_text(&output.pdf).expect("extract snowman fixture");
        assert!(extracted.contains('☃'), "snowman vanished: {extracted:?}");
    } else {
        assert!(
            prof.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::MissingGlyph && reason.detail.contains("U+2603")
            }),
            "unexpected reasons: {:?}",
            prof.reasons
        );
        assert!(matches!(
            convert(&input, OfficeFormat::Docx, &ConvertOptions::default()),
            Err(OfficeError::Unsupported {
                code: UnsupportedCode::MissingGlyph,
                ref detail,
            }) if detail.contains("U+2603")
        ));
    }
}

#[test]
fn docx_header_table_cell_must_be_preserved() {
    let output = convert_fixture(&header_table_docx(), OfficeFormat::Docx);
    let text = extract_text(&output.pdf).expect("extract header-table fixture");

    assert!(
        text.contains("HDRCELL"),
        "header table cell vanished: {text:?}"
    );
}

#[test]
fn docx_arabic_fallback_extracts_logical_source() {
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from(
            "../../packages/doc-yantra-fonts/fonts",
        )],
        ..ConvertOptions::default()
    };
    for (label, source, paragraph_properties) in [
        ("Arabic", "مرحبا بالعالم", "<w:pPr><w:bidi/></w:pPr>"),
        ("mixed fallback", "Latin مرحبا", "<w:pPr><w:bidi/></w:pPr>"),
        ("combining", "Cafe\u{301} semantic cluster", ""),
        (
            "Arabic combining marks",
            "ب\u{064B}\u{064C}",
            "<w:pPr><w:bidi/></w:pPr>",
        ),
        (
            "Hebrew combining marks",
            "ש\u{05B0}\u{05B1}",
            "<w:pPr><w:bidi/></w:pPr>",
        ),
        (
            "Arabic Extended-A mark",
            "ب\u{08CA}\u{08CB}",
            "<w:pPr><w:bidi/></w:pPr>",
        ),
    ] {
        let input = minimal_docx(&format!(
            r#"<w:p>{paragraph_properties}<w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/></w:rPr><w:t>{source}</w:t></w:r></w:p>"#,
        ));
        let output = convert(&input, OfficeFormat::Docx, &options)
            .unwrap_or_else(|error| panic!("{label} fallback: {error}"));
        let extracted =
            extract_text(&output.pdf).unwrap_or_else(|error| panic!("extract {label}: {error}"));
        assert!(
            extracted.contains(source),
            "logical {label} text did not round-trip: {extracted:?}"
        );
    }
}

#[test]
fn pptx_unparseable_slide_must_be_a_typed_error() {
    let input = two_slide_pptx(b"<not-xml");
    for error in [
        profile(&input, OfficeFormat::Pptx).unwrap_err(),
        convert(&input, OfficeFormat::Pptx, &ConvertOptions::default()).unwrap_err(),
    ] {
        assert!(matches!(error, OfficeError::Invalid(ref detail) if detail.contains("slide 2")));
    }
}

#[test]
fn xlsx_unparseable_sheet_must_be_a_typed_error() {
    let input = two_sheet_xlsx(b"<not-xml");
    for error in [
        profile(&input, OfficeFormat::Xlsx).unwrap_err(),
        convert(&input, OfficeFormat::Xlsx, &ConvertOptions::default()).unwrap_err(),
    ] {
        assert!(matches!(error, OfficeError::Invalid(ref detail) if detail.contains("Sheet2")));
    }
}

#[test]
fn pptx_shape_rotation_must_change_raster_output() {
    let plain = convert_fixture(&transformed_shape_pptx(""), OfficeFormat::Pptx);
    let rotated = convert_fixture(
        &transformed_shape_pptx(r#"rot="2700000""#),
        OfficeFormat::Pptx,
    );
    let plain_raster = raster(&plain.pdf).to_rgba8();
    let rotated_raster = raster(&rotated.pdf).to_rgba8();

    assert!(
        plain_raster != rotated_raster,
        "45-degree rotation produced an identical raster"
    );
}

#[test]
fn pptx_rotation_90_and_flip_h_each_change_raster_output() {
    let plain =
        raster(&convert_fixture(&transformed_shape_pptx(""), OfficeFormat::Pptx).pdf).to_rgba8();
    for attributes in [r#"rot="5400000""#, r#"flipH="1""#] {
        let transformed =
            raster(&convert_fixture(&transformed_shape_pptx(attributes), OfficeFormat::Pptx).pdf)
                .to_rgba8();
        assert_ne!(
            plain, transformed,
            "transform {attributes} had no raster effect"
        );
    }
}

#[test]
fn docx_mid_document_section_must_change_second_page_media_box() {
    let body = r#"
<w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Portrait Letter</w:t></w:r></w:p>
<w:p><w:r><w:br w:type="page"/></w:r></w:p>
<w:p><w:r><w:t>Landscape A4</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/></w:sectPr>"#;
    let document = wrap_document(body);
    let input = minimal_docx(body);
    assert!(
        document.contains("16838"),
        "testkit wrapper lost section XML"
    );
    let output = convert_fixture(&input, OfficeFormat::Docx);
    let sizes = page_sizes(&output.pdf).expect("read output MediaBoxes");

    assert!(
        sizes.len() > 1,
        "expected more than one page, got {sizes:?}"
    );
    assert_ne!(
        sizes[0], sizes[1],
        "page 2 MediaBox must differ from page 1: {sizes:?}"
    );
}
