//! Purpose-built DOCX wave-3 fixtures. Each package is assembled in memory so
//! the test keeps the relevant OOXML visible beside its assertions.

use office_core::layout::{Item, Page, PathCommand, Positioned, WordDoc, layout_docx};
use office_core::testkit::{DOC_NAMESPACES, minimal_docx, zip_package};
use office_core::{
    ConvertOptions, OfficeError, OfficeFormat, UnsupportedCode, convert, profile,
    profile_with_options,
};
use sha2::{Digest, Sha256};

fn page_text(page: &Page) -> String {
    fn item_text(item: &Item) -> String {
        match item {
            Item::Glyphs(run) => run.glyphs.iter().map(|glyph| glyph.ch).collect(),
            Item::Clip { items, .. } => items.iter().map(|item| item_text(&item.item)).collect(),
            Item::Transform { item, .. } => item_text(item),
            _ => String::new(),
        }
    }

    page.items
        .iter()
        .map(|item| item_text(&item.item))
        .collect()
}

fn frame_clip(page: &Page) -> (&Positioned, &[Positioned], f64, f64) {
    let positioned = page
        .items
        .iter()
        .find(|positioned| matches!(positioned.item, Item::Clip { .. }))
        .expect("exact-height frame clip");
    let Item::Clip {
        commands, items, ..
    } = &positioned.item
    else {
        unreachable!();
    };
    let mut max_x = 0.0_f64;
    let mut max_y = 0.0_f64;
    for command in commands {
        match command {
            PathCommand::MoveTo { x, y } | PathCommand::LineTo { x, y } => {
                max_x = max_x.max(*x);
                max_y = max_y.max(*y);
            }
            PathCommand::CubicTo { .. } | PathCommand::Close => {}
        }
    }
    (positioned, items, max_x, max_y)
}

fn nested_glyph_baselines(items: &[Positioned]) -> Vec<f64> {
    let mut baselines = Vec::new();
    for positioned in items {
        match &positioned.item {
            Item::Glyphs(_) => baselines.push(positioned.y_pt),
            Item::Clip { items, .. } => baselines.extend(nested_glyph_baselines(items)),
            Item::Transform { .. }
            | Item::SemanticText(_)
            | Item::Rect { .. }
            | Item::Line { .. }
            | Item::Path { .. }
            | Item::Image { .. }
            | Item::CroppedImage { .. }
            | Item::Link { .. } => {}
        }
    }
    baselines
}

fn glyph_positions(items: &[Positioned], origin_x: f64, origin_y: f64) -> Vec<(String, f64, f64)> {
    let mut glyphs = Vec::new();
    for positioned in items {
        match &positioned.item {
            Item::Glyphs(run) => glyphs.push((
                run.glyphs.iter().map(|glyph| glyph.ch).collect(),
                origin_x + positioned.x_pt,
                origin_y + positioned.y_pt,
            )),
            Item::Clip { items, .. } => glyphs.extend(glyph_positions(
                items,
                origin_x + positioned.x_pt,
                origin_y + positioned.y_pt,
            )),
            Item::Transform { .. }
            | Item::SemanticText(_)
            | Item::Rect { .. }
            | Item::Line { .. }
            | Item::Path { .. }
            | Item::Image { .. }
            | Item::CroppedImage { .. }
            | Item::Link { .. } => {}
        }
    }
    glyphs
}

fn assert_profile_convert_agree(bytes: &[u8]) {
    let predicted = profile(bytes, OfficeFormat::Docx).expect("profile");
    let converted = convert(bytes, OfficeFormat::Docx, &ConvertOptions::default());
    assert_eq!(
        predicted.supported,
        converted.is_ok(),
        "{predicted:?} / {converted:?}"
    );
    assert!(
        predicted.supported,
        "unexpected reasons: {:?}",
        predicted.reasons
    );
}

fn assert_profile_convert_reject(bytes: &[u8], expected: UnsupportedCode) {
    let predicted = profile(bytes, OfficeFormat::Docx).expect("profile");
    assert!(
        !predicted.supported,
        "unexpectedly supported: {predicted:?}"
    );
    assert!(
        predicted
            .reasons
            .iter()
            .any(|reason| reason.code == expected),
        "expected {expected:?}, got {:?}",
        predicted.reasons
    );
    assert!(matches!(
        convert(bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(OfficeError::Unsupported { code, .. }) if code == expected
    ));
    let document = WordDoc::open(bytes, &ConvertOptions::default()).expect("open rejected fixture");
    assert!(matches!(
        layout_docx(&document),
        Err(OfficeError::Unsupported { code, .. }) if code == expected
    ));
}

fn package_with_parts(
    body: &str,
    document_rels: &str,
    extra_parts: Vec<(&str, Vec<u8>)>,
) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{body}</w:body></w:document>"#
    );
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpg" ContentType="image/jpeg"/><Default Extension="emf" ContentType="image/x-emf"/><Default Extension="wmf" ContentType="image/x-wmf"/><Default Extension="bin" ContentType="application/vnd.openxmlformats-officedocument.oleObject"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.as_bytes().to_vec();
    let root = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.as_bytes().to_vec();
    let mut owned = vec![
        ("[Content_Types].xml", ct),
        ("_rels/.rels", root),
        ("word/document.xml", document.into_bytes()),
        (
            "word/_rels/document.xml.rels",
            document_rels.as_bytes().to_vec(),
        ),
    ];
    owned.extend(extra_parts);
    let borrowed: Vec<_> = owned
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect();
    zip_package(&borrowed)
}

fn oversized_png_header(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&13_u32.to_be_bytes());
    png.extend_from_slice(&ihdr);
    png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    let mut idat = b"IDAT".to_vec();
    idat.extend_from_slice(&[
        0x78, 0x9c, 0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01,
    ]);
    png.extend_from_slice(&12_u32.to_be_bytes());
    png.extend_from_slice(&idat);
    png.extend_from_slice(&crc32(&idat).to_be_bytes());
    png.extend_from_slice(&0_u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
    png
}

fn assert_docx_image_reason(bytes: &[u8], expected_reason: &str) {
    let prof = profile(bytes, OfficeFormat::Docx).expect("profile image fixture");
    assert!(!prof.supported, "profile accepted {expected_reason}");
    assert!(
        prof.reasons
            .iter()
            .any(|reason| { serde_json::to_value(reason.code).unwrap() == expected_reason }),
        "missing {expected_reason}: {:?}",
        prof.reasons
    );
    let error = convert(bytes, OfficeFormat::Docx, &ConvertOptions::default()).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = error else {
        panic!("conversion did not return typed unsupported: {error:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);

    let direct_error = match WordDoc::open(bytes, &ConvertOptions::default()) {
        Ok(doc) => layout_docx(&doc).unwrap_err(),
        Err(error) => error,
    };
    let office_core::OfficeError::Unsupported { code, .. } = direct_error else {
        panic!("direct DOCX path did not fail typed: {direct_error:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);
}

fn cached_chart_xml() -> &'static str {
    r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:grouping val="stacked"/><c:ser><c:idx val="0"/><c:tx><c:v>DOCX Series</c:v></c:tx><c:cat><c:strLit><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:formatCode>0</c:formatCode><c:pt idx="0"><c:v>11</c:v></c:pt><c:pt idx="1"><c:v>22</c:v></c:pt></c:numLit></c:val><c:dLbls><c:showVal val="1"/></c:dLbls></c:ser></c:barChart><c:catAx><c:axId val="1"/></c:catAx><c:valAx><c:axId val="2"/></c:valAx></c:plotArea></c:chart></c:chartSpace>"#
}

fn docx_with_numbering(format: &str, value: i64) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>numbered</w:t></w:r></w:p></w:body></w:document>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0"?><w:numbering{DOC_NAMESPACES}>
        <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="{value}"/>
        <w:numFmt w:val="{format}"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
    );
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
    ])
}

#[test]
fn fixture_numbering_ordinal_text_renders_and_agrees() {
    let bytes = docx_with_numbering("ordinalText", 21);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("twenty-first."), "{text:?}");
    assert!(text.contains("numbered"), "{text:?}");
}

#[test]
fn fixture_multicolumn_flows_column_to_column_and_agrees() {
    let paragraphs = (0..70)
        .map(|index| format!("<w:p><w:r><w:t>P{index:02}</w:t></w:r></w:p>"))
        .collect::<String>();
    let bytes = minimal_docx(&format!(
        "{paragraphs}<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\"/><w:cols w:num=\"2\"><w:col w:w=\"3000\" w:space=\"720\"/><w:col w:w=\"5400\"/></w:cols></w:sectPr>"
    ));
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let lefts: Vec<i64> = laid.pages[0]
        .items
        .iter()
        .filter(|item| matches!(item.item, Item::Glyphs(_)))
        .map(|item| item.x_pt.round() as i64)
        .collect();
    assert!(
        lefts.iter().any(|x| *x < 200) && lefts.iter().any(|x| *x > 250),
        "{lefts:?}"
    );
}

#[test]
fn fixture_frame_pr_common_position_size_wrap_and_anchor_subset() {
    let cases = [
        (
            "page",
            r#"<w:p><w:pPr><w:framePr w:x="720" w:y="1440" w:w="2880" w:h="720" w:hRule="exact" w:wrap="none" w:hAnchor="page" w:vAnchor="page"/></w:pPr><w:r><w:t>page frame</w:t></w:r></w:p>"#,
            36.0,
            72.0,
        ),
        (
            "margin",
            r#"<w:p><w:pPr><w:framePr w:x="360" w:y="360" w:w="2880" w:h="720" w:hRule="atLeast" w:wrap="notBeside" w:hAnchor="margin" w:vAnchor="margin"/></w:pPr><w:r><w:t>margin frame</w:t></w:r></w:p>"#,
            90.0,
            90.0,
        ),
        (
            "text",
            r#"<w:p><w:pPr><w:framePr w:xAlign="center" w:y="0" w:w="2880" w:h="720" w:hRule="auto" w:wrap="around" w:hAnchor="text" w:vAnchor="text"/></w:pPr><w:r><w:t>text frame</w:t></w:r></w:p>"#,
            234.0,
            72.0,
        ),
    ];

    for (label, body, expected_x, expected_top) in cases {
        let bytes = minimal_docx(body);
        assert_profile_convert_agree(&bytes);
        let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let laid = layout_docx(&doc).unwrap().0;
        let glyph = glyph_positions(&laid.pages[0].items, 0.0, 0.0)
            .into_iter()
            .next()
            .unwrap();
        assert!(
            (glyph.1 - expected_x).abs() < 1.0,
            "{label} x: {} != {expected_x}",
            glyph.1
        );
        assert!(
            glyph.2 >= expected_top && glyph.2 < expected_top + 18.0,
            "{label} y: {} outside frame top {expected_top}",
            glyph.2
        );
    }

    let wrapped = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:x="0" w:y="0" w:w="720" w:h="1440" w:hRule="exact" w:wrap="notBeside" w:hAnchor="text" w:vAnchor="text"/></w:pPr><w:r><w:t>one two three four five six</w:t></w:r></w:p><w:p><w:r><w:t>after frame</w:t></w:r></w:p>"#,
    );
    assert_profile_convert_agree(&wrapped);
    let doc = WordDoc::open(&wrapped, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let ys: Vec<_> = glyph_positions(&laid.pages[0].items, 0.0, 0.0)
        .into_iter()
        .map(|(text, _, y)| (text, y))
        .collect();
    let framed_rows = ys
        .iter()
        .filter(|(text, _)| !text.contains("after frame"))
        .count();
    let after_y = ys
        .iter()
        .find(|(text, _)| text.contains("after frame"))
        .unwrap()
        .1;
    assert!(framed_rows > 1, "frame width did not wrap: {ys:?}");
    assert!(after_y >= 144.0, "notBeside did not exclude frame: {ys:?}");
}

#[test]
fn frame_exact_height_that_fits_keeps_authored_box() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:w="2880" w:h="264" w:hRule="exact" w:wrap="notBeside"/></w:pPr><w:r><w:t>fits</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let (_, items, width, height) = frame_clip(&laid.pages[0]);
    assert!((width - 144.0).abs() < 0.01, "clip width={width}");
    assert!((height - 13.2).abs() < 0.01, "clip height={height}");
    assert_eq!(nested_glyph_baselines(items).len(), 1);
}

#[test]
fn frame_exact_height_overflow_is_clipped_without_growing_box() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:w="720" w:h="264" w:hRule="exact" w:wrap="notBeside"/></w:pPr><w:r><w:t>one two three four five six seven eight</w:t></w:r></w:p><w:p><w:r><w:t>AFTER</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let (clip, items, width, height) = frame_clip(&laid.pages[0]);
    assert!((width - 36.0).abs() < 0.01, "clip width={width}");
    assert!((height - 13.2).abs() < 0.01, "clip height={height}");
    let baselines = nested_glyph_baselines(items);
    assert!(
        baselines.iter().copied().fold(0.0_f64, f64::max) > height,
        "overflowing lines must remain inside bounded clip: {baselines:?}"
    );
    let after = glyph_positions(&laid.pages[0].items, 0.0, 0.0)
        .into_iter()
        .find(|(text, _, _)| text.contains("AFTER"))
        .expect("paragraph after exact frame");
    assert!(
        after.2 < clip.y_pt + 50.0,
        "exact frame exclusion grew with hidden overflow: clip={} after={}",
        clip.y_pt,
        after.2
    );
}

#[test]
fn frame_at_least_height_still_grows_for_overflow() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:w="720" w:h="264" w:hRule="atLeast" w:wrap="notBeside"/></w:pPr><w:r><w:t>one two three four five six seven eight</w:t></w:r></w:p><w:p><w:r><w:t>AFTER</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        !laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Clip { .. })),
        "atLeast content must not be clipped"
    );
    let baselines: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(_) => Some(positioned.y_pt),
            _ => None,
        })
        .collect();
    assert!(baselines.last().unwrap() - baselines.first().unwrap() > 50.0);
}

#[test]
fn frame_auto_height_grows_to_content() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:w="720" w:h="264" w:hRule="auto" w:wrap="notBeside"/></w:pPr><w:r><w:t>one two three four five six seven eight</w:t></w:r></w:p><w:p><w:r><w:t>AFTER</w:t></w:r></w:p>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        !laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Clip { .. })),
        "auto content must not be clipped"
    );
    let baselines: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(_) => Some(positioned.y_pt),
            _ => None,
        })
        .collect();
    assert!(baselines.last().unwrap() - baselines.first().unwrap() > 50.0);
}

#[test]
fn visible_wml_properties_never_disappear_through_property_catch_alls() {
    let assert_refused = |label: &str, body: String| {
        let bytes = minimal_docx(&body);
        let predicted = profile(&bytes, OfficeFormat::Docx).unwrap();
        assert!(
            predicted.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::UnknownElement
                    && reason.detail.contains(label.rsplit('/').next().unwrap())
            }),
            "{label} vanished from profile: {predicted:?}"
        );
        assert!(
            matches!(
                convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
                Err(office_core::OfficeError::Unsupported {
                    code: UnsupportedCode::UnknownElement,
                    ..
                })
            ),
            "{label} conversion did not fail typed"
        );
    };

    for property in [
        "suppressLineNumbers",
        "suppressAutoHyphens",
        "kinsoku",
        "wordWrap",
        "overflowPunct",
        "topLinePunct",
        "autoSpaceDE",
        "autoSpaceDN",
        "adjustRightInd",
        "snapToGrid",
        "mirrorIndents",
        "suppressOverlap",
        "textDirection",
        "textAlignment",
        "textboxTightWrap",
        "unknownVisibleParagraphProperty",
    ] {
        assert_refused(
            &format!("pPr/{property}"),
            format!("<w:p><w:pPr><w:{property}/></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"),
        );
    }
    for property in [
        "bCs",
        "iCs",
        "outline",
        "shadow",
        "emboss",
        "imprint",
        "snapToGrid",
        "w",
        "position",
        "szCs",
        "effect",
        "bdr",
        "fitText",
        "cs",
        "em",
        "eastAsianLayout",
        "specVanish",
        "oMath",
        "unknownVisibleRunProperty",
    ] {
        assert_refused(
            &format!("rPr/{property}"),
            format!("<w:p><w:r><w:rPr><w:{property}/></w:rPr><w:t>x</w:t></w:r></w:p>"),
        );
    }
    for property in [
        "tblCellSpacing",
        "jc",
        "ins",
        "del",
        "unknownVisibleRowProperty",
    ] {
        assert_refused(
            &format!("trPr/{property}"),
            format!(
                "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:trPr><w:{property}/></w:trPr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
            ),
        );
    }
    assert_refused(
        "trPr/trHeight",
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="400" w:hRule="invented"/></w:trPr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string(),
    );
    for property in [
        "hMerge",
        "textDirection",
        "noWrap",
        "tcFitText",
        "hideMark",
        "cellIns",
        "cellDel",
        "cellMerge",
        "unknownVisibleCellProperty",
    ] {
        assert_refused(
            &format!("tcPr/{property}"),
            format!(
                "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:{property}/></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
            ),
        );
    }
    assert_refused(
        "tcPr/vAlign",
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:vAlign w:val="both"/></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string(),
    );
    for property in [
        "tblOverlap",
        "bidiVisual",
        "tblCellSpacing",
        "shd",
        "unknownVisibleTableProperty",
    ] {
        assert_refused(
            &format!("tblPr/{property}"),
            format!(
                "<w:tbl><w:tblPr><w:{property}/></w:tblPr><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
            ),
        );
    }
    for (name, property) in [
        ("tblLayout", r#"<w:tblLayout w:type="invented"/>"#),
        ("jc", r#"<w:jc w:val="both"/>"#),
    ] {
        assert_refused(
            &format!("tblPr/{name}"),
            format!(
                "<w:tbl><w:tblPr>{property}</w:tblPr><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
            ),
        );
    }
    for property in [
        "footnotePr",
        "endnotePr",
        "lnNumType",
        "noEndnote",
        "textDirection",
        "bidi",
        "rtlGutter",
        "footnoteColumns",
        "unknownVisibleSectionProperty",
    ] {
        assert_refused(
            &format!("sectPr/{property}"),
            format!("<w:p><w:r><w:t>x</w:t></w:r></w:p><w:sectPr><w:{property}/></w:sectPr>"),
        );
    }
    // A grid-less `w:docGrid` (Word stamps one on nearly every section) paints
    // nothing and is supported; the East-Asian grid types still are not.
    for (name, property) in [
        (
            "docGrid",
            r#"<w:docGrid w:type="lines" w:linePitch="360"/>"#,
        ),
        (
            "docGrid",
            r#"<w:docGrid w:type="snapToChars" w:linePitch="360"/>"#,
        ),
        ("vAlign", r#"<w:vAlign w:val="both"/>"#),
        ("type", r#"<w:type w:val="nextColumn"/>"#),
        ("cols", r#"<w:cols w:sep="1"/>"#),
        ("pgNumType", r#"<w:pgNumType w:chapStyle="1"/>"#),
    ] {
        assert_refused(
            &format!("sectPr/{name}"),
            format!("<w:p><w:r><w:t>x</w:t></w:r></w:p><w:sectPr>{property}</w:sectPr>"),
        );
    }
}

#[test]
fn explicitly_nonvisual_wml_property_allowlist_stays_supported() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:divId w:val="1"/><w:pPrChange/></w:pPr><w:r><w:rPr><w:lang w:val="en-US"/><w:noProof/><w:webHidden/><w:rPrChange/></w:rPr><w:t>x</w:t></w:r></w:p>
        <w:tbl><w:tblPr><w:tblCaption w:val="caption"/><w:tblDescription w:val="description"/><w:tblPrChange/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:trPr><w:divId w:val="2"/><w:trPrChange/></w:trPr><w:tc><w:tcPr><w:tcPrChange/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        <w:sectPr><w:paperSrc/><w:formProt/><w:printerSettings/><w:sectPrChange/></w:sectPr>"#,
    );
    assert_profile_convert_agree(&bytes);
}

#[test]
fn visible_properties_in_styles_and_headers_are_profiled_before_convert() {
    let empty_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles{DOC_NAMESPACES}><w:style w:type="paragraph" w:styleId="Risky"><w:pPr><w:snapToGrid/></w:pPr><w:rPr><w:emboss/></w:rPr></w:style></w:styles>"#
    );
    let styled = package_with_parts(
        r#"<w:p><w:pPr><w:pStyle w:val="Risky"/></w:pPr><w:r><w:t>styled</w:t></w:r></w:p>"#,
        empty_rels,
        vec![("word/styles.xml", styles.into_bytes())],
    );
    let styled_profile = profile(&styled, OfficeFormat::Docx).unwrap();
    assert!(styled_profile.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::UnknownElement
            && (reason.detail.contains("snapToGrid") || reason.detail.contains("emboss"))
    }));
    assert!(matches!(
        convert(&styled, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::UnknownElement,
            ..
        })
    ));

    let header_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
    let header = format!(
        r#"<?xml version="1.0"?><w:hdr{DOC_NAMESPACES}><w:p><w:r><w:rPr><w:imprint/></w:rPr><w:t>header</w:t></w:r></w:p></w:hdr>"#
    );
    let header_doc = package_with_parts(
        r#"<w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/></w:sectPr>"#,
        header_rels,
        vec![("word/header1.xml", header.into_bytes())],
    );
    let header_profile = profile(&header_doc, OfficeFormat::Docx).unwrap();
    assert!(header_profile.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::UnknownElement && reason.detail.contains("imprint")
    }));
    assert!(matches!(
        convert(&header_doc, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::UnknownElement,
            ..
        })
    ));
}

#[test]
fn table_row_grid_before_after_and_preferred_widths_render_shared_grid() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/></w:tblGrid>
        <w:tr><w:trPr><w:gridBefore w:val="1"/><w:wBefore w:w="2000" w:type="dxa"/><w:gridAfter w:val="1"/><w:wAfter w:w="3000" w:type="dxa"/></w:trPr>
          <w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>X</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr>
          <w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
          <w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
          <w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc>
        </w:tr></w:tbl>"#,
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let glyphs = glyph_positions(&laid.pages[0].items, 0.0, 0.0);
    let x = |needle: &str| {
        glyphs
            .iter()
            .find(|(text, _, _)| text.contains(needle))
            .map(|(_, x, _)| *x)
            .unwrap()
    };
    assert!((x("X") - x("B")).abs() < 0.1, "glyphs={glyphs:?}");
    assert!(x("B") - x("A") > 90.0, "wBefore not applied: {glyphs:?}");
    assert!(
        x("C") - x("B") > 40.0,
        "gridAfter shifted cells: {glyphs:?}"
    );
}

#[test]
fn fixed_table_without_grid_uses_authored_cell_widths() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr><w:tr>
        <w:tc><w:tcPr><w:tcW w:w="1000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>NARROW</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>WIDE</w:t></w:r></w:p></w:tc>
        </w:tr></w:tbl>"#,
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let glyphs = glyph_positions(&laid.pages[0].items, 0.0, 0.0);
    let narrow_x = glyphs
        .iter()
        .find(|(text, _, _)| text.starts_with('N'))
        .unwrap_or_else(|| panic!("NARROW glyph run missing: {glyphs:?}"))
        .1;
    let wide_x = glyphs
        .iter()
        .find(|(text, _, _)| text.contains("WIDE"))
        .unwrap_or_else(|| panic!("WIDE glyph run missing: {glyphs:?}"))
        .1;
    assert!(wide_x - narrow_x < 70.0, "fixed widths ignored: {glyphs:?}");
}

#[test]
fn cell_margin_and_vertical_alignment_override_table_defaults() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:trPr><w:trHeight w:val="1000" w:hRule="exact"/></w:trPr><w:tc><w:tcPr>
        <w:tcMar><w:left w:w="400" w:type="dxa"/><w:top w:w="200" w:type="dxa"/></w:tcMar><w:vAlign w:val="bottom"/>
        </w:tcPr><w:p><w:r><w:t>LOW</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let low = glyph_positions(&laid.pages[0].items, 0.0, 0.0)
        .into_iter()
        .find(|(text, _, _)| text.contains("LOW"))
        .unwrap();
    assert!(low.1 >= 92.0, "tcMar left override ignored: {low:?}");
    assert!(low.2 >= 110.0, "bottom vAlign ignored: {low:?}");
}

#[test]
fn bottom_cell_margin_bounds_exact_row_alignment_and_clip() {
    let layout = |bottom_twips: u32| {
        let bytes = minimal_docx(&format!(
            r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
            <w:tr><w:trPr><w:trHeight w:val="1000" w:hRule="exact"/></w:trPr><w:tc><w:tcPr>
            <w:tcMar><w:bottom w:w="{bottom_twips}" w:type="dxa"/></w:tcMar><w:vAlign w:val="bottom"/>
            </w:tcPr><w:p><w:r><w:t>LOW</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        ));
        let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        layout_docx(&doc).unwrap().0
    };

    let without_margin = layout(0);
    let with_margin = layout(400);
    let baseline = |page: &Page| {
        glyph_positions(&page.items, 0.0, 0.0)
            .into_iter()
            .find(|(text, _, _)| text.contains("LOW"))
            .unwrap()
            .2
    };
    let without_y = baseline(&without_margin.pages[0]);
    let with_y = baseline(&with_margin.pages[0]);
    assert!(
        ((without_y - with_y) - 20.0).abs() < 0.1,
        "bottom margin must move bottom-aligned content up by 20pt: without={without_y}, with={with_y}"
    );
    let (_, _, _, clip_bottom) = frame_clip(&with_margin.pages[0]);
    assert!(
        (clip_bottom - 30.0).abs() < 0.01,
        "exact-row clip must stop before the 20pt bottom inset: {clip_bottom}"
    );
}

fn horizontal_table_border_widths(page: &Page) -> Vec<f64> {
    let mut widths: Vec<f64> = page
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Line {
                dx_pt,
                dy_pt,
                stroke,
            } if dx_pt.abs() > 0.01 && dy_pt.abs() < 0.01 => Some(stroke.width_pt),
            _ => None,
        })
        .collect();
    widths.sort_by(f64::total_cmp);
    widths
}

fn vertical_table_borders(page: &Page) -> Vec<(f64, f64, f64)> {
    let mut borders = page
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Line {
                dx_pt,
                dy_pt,
                stroke,
            } if dx_pt.abs() < 0.01 && dy_pt.abs() > 0.01 => {
                Some((positioned.x_pt, *dy_pt, stroke.width_pt))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    borders.sort_by(|left, right| left.0.total_cmp(&right.0));
    borders
}

#[test]
fn vertical_table_borders_choose_one_shared_edge_and_merge_across_rows() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="8"/><w:bottom w:val="single" w:sz="8"/>
        <w:left w:val="single" w:sz="8"/><w:right w:val="single" w:sz="8"/>
        <w:insideV w:val="single" w:sz="4"/><w:insideH w:val="single" w:sz="8"/>
        </w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:right w:val="single" w:sz="16" w:color="111111"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr><w:tcBorders><w:left w:val="single" w:sz="8" w:color="EEEEEE"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:right w:val="single" w:sz="16" w:color="111111"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
        <w:tc><w:tcPr><w:tcBorders><w:left w:val="single" w:sz="8" w:color="EEEEEE"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let borders = vertical_table_borders(&laid.pages[0]);

    assert_eq!(
        borders.len(),
        3,
        "two rows must collapse into one run for each outer/shared vertical seam: {borders:?}"
    );
    assert!(
        (borders[1].2 - 2.0).abs() < 0.01,
        "the wider left-cell edge must win the shared seam: {borders:?}"
    );
}

#[test]
fn centered_table_alignment_uses_the_asymmetric_painted_border_footprint() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:jc w:val="center"/><w:tblW w:w="2000" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="8"/><w:bottom w:val="single" w:sz="8"/>
        <w:left w:val="single" w:sz="32"/><w:right w:val="single" w:sz="16"/>
        </w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:p><w:r><w:t>CENTERED</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let borders = vertical_table_borders(&laid.pages[0]);
    assert_eq!(borders.len(), 2, "expected two outer seams: {borders:?}");
    let painted_left = borders[0].0 - borders[0].2 / 2.0;
    let painted_right = borders[1].0 + borders[1].2 / 2.0;
    assert!(
        ((painted_left + painted_right) / 2.0 - 306.0).abs() < 0.01,
        "the painted footprint, not the border centerline grid, must be centered: {borders:?}"
    );
}

#[test]
fn horizontal_table_borders_choose_context_and_paint_shared_edge_once() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="16" w:color="111111"/>
        <w:bottom w:val="single" w:sz="24" w:color="222222"/>
        <w:insideH w:val="single" w:sz="4" w:color="333333"/>
        </w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:bottom w:val="single" w:sz="16" w:color="AA0000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>ABOVE</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:top w:val="single" w:sz="8" w:color="0000AA"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>BELOW</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(
        horizontal_table_border_widths(&laid.pages[0]),
        vec![2.0, 2.0, 3.0],
        "outer edges must use top/bottom, while the wider cell edge wins the one shared seam"
    );
}

#[test]
fn explicit_none_and_nil_block_table_fallback_without_reserving_height() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="8" w:color="111111"/>
        <w:bottom w:val="single" w:sz="8" w:color="222222"/>
        <w:insideH w:val="single" w:sz="32" w:color="333333"/>
        </w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:bottom w:val="none"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>ABOVE</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:tcBorders><w:top w:val="nil"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>BELOW</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(
        horizontal_table_border_widths(&laid.pages[0]),
        vec![1.0, 1.0],
        "explicit suppression must remove the interior seam without replacing outer borders"
    );
}

#[test]
fn vertical_merge_suppresses_internal_seam_but_keeps_outer_bottom() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="8" w:color="111111"/>
        <w:bottom w:val="single" w:sz="8" w:color="222222"/>
        <w:insideH w:val="single" w:sz="40" w:color="333333"/>
        </w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>MERGED</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(
        horizontal_table_border_widths(&laid.pages[0]),
        vec![1.0, 1.0],
        "the 5pt insideH border must neither paint nor reserve a phantom merged-cell seam"
    );
}

#[test]
fn spanning_preferred_width_preserves_an_already_satisfied_unequal_grid() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>SPAN</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:tc><w:p><w:r><w:t>FIRST</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>SECOND</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let glyphs = glyph_positions(&laid.pages[0].items, 0.0, 0.0);
    let first_x = glyphs
        .iter()
        .find(|(text, _, _)| text.contains("FIRST"))
        .unwrap()
        .1;
    let second_x = glyphs
        .iter()
        .find(|(text, _, _)| text.contains("SECOND"))
        .unwrap()
        .1;
    assert!(
        ((second_x - first_x) - 50.0).abs() < 0.1,
        "a satisfied 150pt span must preserve its 50pt + 100pt grid: {glyphs:?}"
    );
}

#[test]
fn thick_border_insets_only_the_cell_columns_it_covers() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr>
        <w:tc><w:tcPr><w:tcBorders><w:top w:val="single" w:sz="64" w:color="000000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>THICK</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>PLAIN</w:t></w:r></w:p></w:tc>
        </w:tr></w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let glyphs = glyph_positions(&laid.pages[0].items, 0.0, 0.0);
    let thick_y = glyphs
        .iter()
        .find(|(text, _, _)| text.contains("THICK"))
        .unwrap()
        .2;
    let plain_y = glyphs
        .iter()
        .find(|(text, _, _)| text.contains("PLAIN"))
        .unwrap()
        .2;
    assert!(
        ((thick_y - plain_y) - 4.0).abs() < 0.1,
        "only the bordered cell should receive the 4pt half-stroke inset: {glyphs:?}"
    );
}

#[test]
fn unresolved_table_grid_style_does_not_fabricate_borders() {
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblLayout w:type="fixed"/></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:p><w:r><w:t>NO BORDER</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        laid.pages[0]
            .items
            .iter()
            .all(|positioned| !matches!(positioned.item, Item::Line { .. })),
        "an unresolved style id contributes no formatting"
    );
}

#[test]
fn table_row_height_exact_clips_but_at_least_and_auto_grow() {
    let table = |rule: &str| {
        minimal_docx(&format!(
            r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="720"/></w:tblGrid>
            <w:tr><w:trPr><w:trHeight w:val="360" w:hRule="{rule}"/></w:trPr><w:tc><w:p><w:r><w:t>one two three four five six</w:t></w:r></w:p></w:tc></w:tr>
            <w:tr><w:tc><w:p><w:r><w:t>AFTER</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        ))
    };

    let exact = table("exact");
    assert_profile_convert_agree(&exact);
    let exact_doc = WordDoc::open(&exact, &ConvertOptions::default()).unwrap();
    let exact_laid = layout_docx(&exact_doc).unwrap().0;
    let (_, _, _, clip_height) = frame_clip(&exact_laid.pages[0]);
    assert!((clip_height - 18.0).abs() < 0.01);
    let exact_after = glyph_positions(&exact_laid.pages[0].items, 0.0, 0.0)
        .into_iter()
        .find(|(text, _, _)| text.starts_with('A'))
        .unwrap_or_else(|| panic!("exact AFTER glyph run missing"))
        .2;

    for rule in ["atLeast", "auto"] {
        let bytes = table(rule);
        assert_profile_convert_agree(&bytes);
        let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let laid = layout_docx(&doc).unwrap().0;
        let after = glyph_positions(&laid.pages[0].items, 0.0, 0.0)
            .into_iter()
            .find(|(text, _, _)| text.starts_with('A'))
            .unwrap_or_else(|| panic!("{rule} AFTER glyph run missing"))
            .2;
        assert!(
            after > exact_after + 20.0,
            "{rule} row did not grow: exact={exact_after} actual={after}"
        );
    }
}

#[test]
fn hidden_table_row_neither_paints_nor_makes_profile_pessimistic() {
    let bytes = minimal_docx(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr><w:trPr><w:hidden/></w:trPr><w:tc><w:p><w:r><w:t>HIDDEN \u{0378}</w:t></w:r></w:p></w:tc></w:tr>\
         <w:tr><w:tc><w:p><w:r><w:t>VISIBLE</w:t></w:r></w:p></w:tc></w:tr></w:tbl>",
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let text = page_text(&laid.pages[0]);
    assert!(
        text.contains("VISIBLE") && !text.contains("HIDDEN"),
        "{text:?}"
    );
}

#[test]
fn fixture_frame_pr_drop_cap_outside_subset_is_typed() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:framePr w:dropCap="drop" w:lines="3"/></w:pPr><w:r><w:t>Drop cap</w:t></w:r></w:p>"#,
    );
    let predicted = profile(&bytes, OfficeFormat::Docx).unwrap();
    assert!(!predicted.supported);
    assert!(
        predicted
            .reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::TextBox),
        "{:?}",
        predicted.reasons
    );
    let error = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()).unwrap_err();
    assert!(matches!(
        error,
        office_core::OfficeError::Unsupported {
            code: UnsupportedCode::TextBox,
            ..
        }
    ));
}

#[test]
fn fixture_even_header_requires_even_and_odd_setting() {
    let paragraphs = "<w:p><w:r><w:t>body</w:t></w:r></w:p>".repeat(100);
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{paragraphs}
        <w:sectPr><w:headerReference w:type="default" r:id="rIdD"/><w:headerReference w:type="even" r:id="rIdE"/></w:sectPr>
        </w:body></w:document>"#
    );
    let default_header = format!(
        r#"<?xml version="1.0"?><w:hdr{DOC_NAMESPACES}><w:p><w:r><w:t>DEFAULT</w:t></w:r></w:p></w:hdr>"#
    );
    let even_header = format!(
        r#"<?xml version="1.0"?><w:hdr{DOC_NAMESPACES}><w:p><w:r><w:t>EVEN</w:t></w:r></w:p></w:hdr>"#
    );
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
    let root = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdD" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rIdE" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/></Relationships>"#;
    let bytes = zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/header1.xml", default_header.as_bytes()),
        ("word/header2.xml", even_header.as_bytes()),
    ]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(laid.pages.len() >= 2);
    assert!(page_text(&laid.pages[1]).contains("DEFAULT"));
    assert!(!page_text(&laid.pages[1]).contains("EVEN"));
}

#[test]
fn fixture_footnote_and_endnote_render_and_agree() {
    let body = r#"<w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="1"/><w:endnoteReference w:id="2"/></w:r></w:p>"#;
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="1"><w:p><w:r><w:t>Footnote body</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="2"><w:p><w:r><w:t>Endnote body</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(page_text(&laid.pages[0]).contains("Footnote body"));
    assert!(page_text(laid.pages.last().unwrap()).contains("Endnote body"));
}

fn assert_note_marker_is_inline(page: &Page, note_text: &str) {
    let glyphs = glyph_positions(&page.items, 0.0, 0.0);
    let note = glyphs
        .iter()
        .find(|(text, ..)| text.contains(note_text))
        .unwrap_or_else(|| panic!("missing note text {note_text:?}: {glyphs:?}"));
    let marker = glyphs
        .iter()
        .filter(|(text, x, ..)| text.trim() == "1" && *x <= note.1 + 0.01)
        .min_by(|left, right| (left.2 - note.2).abs().total_cmp(&(right.2 - note.2).abs()))
        .unwrap_or_else(|| panic!("missing marker before {note_text:?}: {glyphs:?}"));

    assert!(
        (marker.2 - note.2).abs() <= 6.0,
        "marker and {note_text:?} must share one line: marker={marker:?}, text={note:?}"
    );
    assert!(
        page_text(page).contains(&format!("1 {note_text}")),
        "marker and note text must remain adjacent in extraction: {:?}",
        page_text(page)
    );
}

#[test]
fn authored_footnote_and_endnote_placeholders_resolve_inline() {
    let body = r#"<w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="7"/><w:endnoteReference w:id="9"/></w:r></w:p>"#;
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="7"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Foot inline</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="9"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:endnoteRef/></w:r><w:r><w:t xml:space="preserve"> End inline</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    );

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_note_marker_is_inline(&laid.pages[0], "Foot inline");
    assert_note_marker_is_inline(laid.pages.last().unwrap(), "End inline");
}

#[test]
fn absent_footnote_and_endnote_placeholders_get_inline_fallbacks() {
    let body = r#"<w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="7"/><w:endnoteReference w:id="9"/></w:r></w:p>"#;
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="7"><w:p><w:r><w:t>Foot fallback</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="9"><w:p><w:r><w:t>End fallback</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    );

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_note_marker_is_inline(&laid.pages[0], "Foot fallback");
    assert_note_marker_is_inline(laid.pages.last().unwrap(), "End fallback");
}

#[test]
fn footnote_reservation_uses_measured_height_and_bottom_anchors_the_note_block() {
    let geometry = |line_count: usize| {
        let body =
            r#"<w:p><w:r><w:t>Body reference</w:t><w:footnoteReference w:id="7"/></w:r></w:p>"#;
        let note_lines = (0..line_count)
            .map(|index| {
                let br = if index + 1 < line_count {
                    "<w:br/>"
                } else {
                    ""
                };
                format!(r#"<w:t>Foot line {index}</w:t>{br}"#)
            })
            .collect::<String>();
        let footnotes = format!(
            r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="7"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r>{note_lines}</w:r></w:p></w:footnote></w:footnotes>"#
        );
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
        let bytes = package_with_parts(
            body,
            rels,
            vec![("word/footnotes.xml", footnotes.into_bytes())],
        );
        let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let laid = layout_docx(&doc).unwrap().0;
        let page = &laid.pages[0];
        let separator_y = page
            .items
            .iter()
            .find_map(|positioned| match &positioned.item {
                Item::Line { dx_pt, dy_pt, .. }
                    if (*dx_pt - 36.0).abs() < 0.01 && dy_pt.abs() < 0.01 =>
                {
                    Some(positioned.y_pt)
                }
                _ => None,
            })
            .expect("footnote separator");
        let glyphs = glyph_positions(&page.items, 0.0, 0.0);
        let body_y = glyphs
            .iter()
            .find(|(text, ..)| text.contains("Body reference"))
            .expect("body reference")
            .2;
        let last_y = glyphs
            .iter()
            .find(|(text, ..)| text.contains(&format!("Foot line {}", line_count - 1)))
            .expect("last footnote line")
            .2;
        (separator_y, body_y, last_y)
    };

    let short = geometry(1);
    let long = geometry(3);
    assert!(
        short.0 - long.0 > 20.0,
        "a three-line note must reserve more body height than a one-line note: short={short:?}, long={long:?}"
    );
    assert!(
        (short.2 - long.2).abs() < 0.1,
        "both note blocks must bottom-align to the same content bottom: short={short:?}, long={long:?}"
    );
    assert!(
        short.1 < short.0 && long.1 < long.0,
        "body and notes may not overlap"
    );
}

#[test]
fn note_markers_follow_first_reference_order_per_kind() {
    let body = r#"<w:p><w:r><w:t>A</w:t><w:footnoteReference w:id="0"/><w:t>B</w:t><w:footnoteReference w:id="7"/><w:t>E</w:t><w:endnoteReference w:id="42"/></w:r></w:p>"#;
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="0"><w:p><w:r><w:t>Footnote zero</w:t></w:r></w:p></w:footnote><w:footnote w:id="7"><w:p><w:r><w:t>Footnote seven</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="42"><w:p><w:r><w:t>Endnote forty-two</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    );

    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let first_page = page_text(&laid.pages[0]);
    let last_page = page_text(laid.pages.last().unwrap());

    assert!(
        first_page.contains("A1B2E1"),
        "inline markers: {first_page:?}"
    );
    assert!(
        first_page.contains("1 Footnote zero") && first_page.contains("2 Footnote seven"),
        "footnote body markers: {first_page:?}"
    );
    assert!(
        last_page.contains("1 Endnote forty-two"),
        "endnote body marker: {last_page:?}"
    );
}

#[test]
fn table_note_references_participate_in_mixed_first_reference_order() {
    let body = r#"
        <w:p><w:r><w:t>Before</w:t><w:footnoteReference w:id="90"/></w:r></w:p>
        <w:tbl><w:tblGrid><w:gridCol w:w="2500"/><w:gridCol w:w="2500"/></w:tblGrid><w:tr>
          <w:tc><w:tcPr/><w:p><w:r><w:t>CellA</w:t><w:footnoteReference w:id="7"/><w:t>E</w:t><w:endnoteReference w:id="42"/></w:r></w:p></w:tc>
          <w:tc><w:tcPr/><w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>Nested</w:t><w:footnoteReference w:id="33"/><w:t>E</w:t><w:endnoteReference w:id="17"/></w:r></w:p></w:tc></w:tr></w:tbl></w:tc>
        </w:tr></w:tbl>
        <w:p><w:r><w:t>After</w:t><w:footnoteReference w:id="1"/><w:t>E</w:t><w:endnoteReference w:id="99"/></w:r></w:p>
    "#;
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="90"><w:p><w:r><w:t>Foot ninety</w:t></w:r></w:p></w:footnote><w:footnote w:id="7"><w:p><w:r><w:t>Foot seven</w:t></w:r></w:p></w:footnote><w:footnote w:id="33"><w:p><w:r><w:t>Foot thirty-three</w:t></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Foot one</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let endnotes = format!(
        r#"<?xml version="1.0"?><w:endnotes{DOC_NAMESPACES}><w:endnote w:id="42"><w:p><w:r><w:t>End forty-two</w:t></w:r></w:p></w:endnote><w:endnote w:id="17"><w:p><w:r><w:t>End seventeen</w:t></w:r></w:p></w:endnote><w:endnote w:id="99"><w:p><w:r><w:t>End ninety-nine</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
        ],
    );

    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let first_page = page_text(&laid.pages[0]);
    let all_text = laid.pages.iter().map(page_text).collect::<String>();
    assert!(
        first_page.contains("Before1"),
        "before marker: {first_page:?}"
    );
    assert!(
        first_page.contains("CellA2E1"),
        "cell markers: {first_page:?}"
    );
    assert!(
        first_page.contains("Nested3E2"),
        "nested markers: {first_page:?}"
    );
    assert!(
        first_page.contains("After4E3"),
        "after markers: {first_page:?}"
    );
    for expected in [
        "1 Foot ninety",
        "2 Foot seven",
        "3 Foot thirty-three",
        "4 Foot one",
        "1 End forty-two",
        "2 End seventeen",
        "3 End ninety-nine",
    ] {
        assert!(
            all_text.contains(expected),
            "missing {expected:?}: {all_text:?}"
        );
    }
}

#[test]
fn repeated_table_headers_do_not_duplicate_logical_note_references() {
    let rows = (0..80)
        .map(|index| {
            format!(r#"<w:tr><w:trPr><w:trHeight w:val="360" w:hRule="atLeast"/></w:trPr><w:tc><w:tcPr/><w:p><w:r><w:t>Row {index}</w:t></w:r></w:p></w:tc></w:tr>"#)
        })
        .collect::<String>();
    let body = format!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:tcPr/><w:p><w:r><w:t>Header</w:t><w:footnoteReference w:id="7"/></w:r></w:p></w:tc></w:tr>{rows}</w:tbl>"#
    );
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="7"><w:p><w:r><w:t>Header body</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        &body,
        rels,
        vec![("word/footnotes.xml", footnotes.into_bytes())],
    );

    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(laid.pages.len() >= 3, "pages={}", laid.pages.len());
    let all_text = laid.pages.iter().map(page_text).collect::<String>();
    assert_eq!(all_text.matches("Header body").count(), 1, "{all_text:?}");
    assert!(all_text.contains("1 Header body"), "{all_text:?}");
}

#[test]
fn fixture_footnote_overflow_continues_on_following_page() {
    let body = r#"<w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    let note_body = (0..80)
        .map(|index| format!("<w:p><w:r><w:t>Footnote continuation {index}</w:t></w:r></w:p>"))
        .collect::<String>();
    let footnotes = format!(
        r#"<?xml version="1.0"?><w:footnotes{DOC_NAMESPACES}><w:footnote w:id="1">{note_body}</w:footnote></w:footnotes>"#
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![("word/footnotes.xml", footnotes.into_bytes())],
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(laid.pages.len() > 1);
    assert!(page_text(laid.pages.last().unwrap()).contains("Footnote continuation 79"));
}

#[test]
fn fixture_textbox_and_wordart_none_render_and_agree() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="1828800" cy="914400"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:spPr><a:solidFill><a:srgbClr val="FFF2CC"/></a:solidFill></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Box text</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr><a:prstTxWarp prst="none"/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(page_text(&laid.pages[0]).contains("Box text"));
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::Rect { .. }))
    );
}

#[test]
fn fixture_anchor_square_and_crop_render_and_agree() {
    let body = r#"<w:p><w:r><w:drawing><wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapSquare wrapText="bothSides"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:srcRect l="10000" r="10000"/><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing><w:t>Wrapped text beside image</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(body, rels, vec![("word/media/image1.png", image)]);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::CroppedImage { .. }))
    );
    assert!(page_text(&laid.pages[0]).contains("Wrapped text"));
}

#[test]
fn anchor_alignment_ignores_disabled_simple_position() {
    let body = r#"<w:p><w:r><w:drawing><wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>right</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(body, rels, vec![("word/media/image1.png", image)]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let image_x = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| {
            matches!(positioned.item, Item::Image { .. }).then_some(positioned.x_pt)
        })
        .expect("anchored image");
    assert!((image_x - 468.0).abs() <= 0.01, "image_x={image_x}");
}

#[test]
fn anchor_false_boolean_spelling_paints_in_front_of_text() {
    let body = r#"<w:p><w:r><w:drawing><wp:anchor simplePos="false" relativeHeight="7" behindDoc="false" allowOverlap="true"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>right</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing><w:t>Layered text</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(body, rels, vec![("word/media/image1.png", image)]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let image_index = laid.pages[0]
        .items
        .iter()
        .position(|item| matches!(item.item, Item::Image { .. }))
        .expect("image");
    let glyph_index = laid.pages[0]
        .items
        .iter()
        .position(|item| matches!(item.item, Item::Glyphs(_)))
        .expect("text");
    assert!(
        image_index > glyph_index,
        "behindDoc=false must be an overlay"
    );
}

#[test]
fn unsupported_anchor_contracts_and_nested_anchors_fail_typed() {
    let anchors = [
        r#"<wp:anchor simplePos="1" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapThrough wrapText="bothSides"><wp:wrapPolygon><wp:start x="0" y="0"/><wp:lineTo x="21600" y="21600"/></wp:wrapPolygon></wp:wrapThrough><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="0"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1" hidden="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapSquare/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
        r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapTight wrapText="bothSides"><wp:wrapPolygon><wp:start x="100" y="100"/><wp:lineTo x="100" y="20000"/><wp:lineTo x="20000" y="20000"/><wp:lineTo x="20000" y="100"/><wp:lineTo x="100" y="100"/></wp:wrapPolygon></wp:wrapTight><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#,
    ];
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    for anchor in anchors {
        let body = format!("<w:p><w:r><w:drawing>{anchor}</w:drawing></w:r></w:p>");
        let bytes = package_with_parts(&body, rels, vec![("word/media/image1.png", image.clone())]);
        assert_profile_convert_reject(&bytes, UnsupportedCode::AnchoredDrawing);
    }

    let anchor = r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#;
    let body = format!(
        "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2400\"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:drawing>{anchor}</w:drawing></w:r></w:p></w:tc></w:tr></w:tbl>"
    );
    let bytes = package_with_parts(&body, rels, vec![("word/media/image1.png", image)]);
    assert_profile_convert_reject(&bytes, UnsupportedCode::AnchoredDrawing);
}

#[test]
fn fractional_simple_position_coordinates_remain_outside_integral_contract() {
    let anchor = r#"<wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0.5" y="0"/><wp:positionH relativeFrom="margin"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapNone/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor>"#;
    let body = format!("<w:p><w:r><w:drawing>{anchor}</w:drawing></w:r></w:p>");
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(&body, rels, vec![("word/media/image1.png", image)]);
    assert_profile_convert_reject(&bytes, UnsupportedCode::AnchoredDrawing);
}

#[test]
fn overlapping_wrap_anchors_are_rejected_by_profile_and_layout() {
    let anchor = |side: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>{side}</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="2743200"/><wp:wrapSquare wrapText="bothSides"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing><w:t>short</w:t></w:r></w:p>"#
        )
    };
    let body = format!("{}{}", anchor("left"), anchor("right"));
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(&body, rels, vec![("word/media/image1.png", image)]);
    assert_profile_convert_reject(&bytes, UnsupportedCode::AnchoredDrawing);
}

#[test]
fn vml_shapes_in_nested_stories_fail_the_same_profile_contract() {
    let body = r##"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:tcPr/><w:p><w:r><w:pict><v:rect style="position:absolute;left:0pt;top:0pt;width:72pt;height:36pt" fillcolor="#DDEBF7"/></w:pict></w:r></w:p></w:tc></w:tr></w:tbl>"##;
    let bytes = minimal_docx(body);
    assert_profile_convert_reject(&bytes, UnsupportedCode::AnchoredDrawing);
}

#[test]
fn picture_drawings_require_a_positive_extent_in_profile_and_layout() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(body, rels, vec![("word/media/image1.png", image)]);
    assert_profile_convert_reject(&bytes, UnsupportedCode::MissingRelationshipTarget);
}

#[test]
fn inline_image_auto_line_spacing_does_not_scale_object_height() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="3822855"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:p><w:r><w:t>After image</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles{DOC_NAMESPACES}><w:docDefaults><w:rPrDefault/><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults></w:styles>"#
    );
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/media/image1.png", image),
            ("word/styles.xml", styles.into_bytes()),
        ],
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(laid.pages.len(), 1);
    let image_y = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| {
            matches!(positioned.item, Item::Image { .. }).then_some(positioned.y_pt)
        })
        .expect("inline image");
    let after_y = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Glyphs(run)
                if run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.ch)
                    .filter(|character| *character != '\0')
                    .collect::<String>()
                    .contains("After") =>
            {
                Some(positioned.y_pt)
            }
            _ => None,
        })
        .expect("following paragraph");
    assert!(
        after_y - image_y < 325.0,
        "automatic text spacing scaled the 301pt image line: image_y={image_y}, after_y={after_y}"
    );
}

#[test]
fn paragraph_space_before_is_suppressed_at_fresh_page_top() {
    let body = r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:pPr><w:spacing w:before="480"/></w:pPr><w:r><w:t>Page-top heading</w:t></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(laid.pages.len(), 2);
    let baseline = laid.pages[1]
        .items
        .iter()
        .find_map(|positioned| {
            matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
        })
        .expect("page-top heading");
    assert!(
        baseline < 100.0,
        "space-before leaked onto page top: {baseline}"
    );
}

#[test]
fn square_anchor_float_clearance_survives_a_fresh_page_top() {
    let body = r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:r><w:drawing><wp:anchor simplePos="0" relativeHeight="1" behindDoc="0" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>left</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/><wp:wrapSquare wrapText="bothSides"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing><w:t>Wrapped page-top text</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let image = std::fs::read("../mock-files/png/file1.png").unwrap();
    let bytes = package_with_parts(body, rels, vec![("word/media/image1.png", image)]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let image_y = laid.pages[1]
        .items
        .iter()
        .find_map(|item| matches!(item.item, Item::Image { .. }).then_some(item.y_pt))
        .expect("page-top anchor image");
    assert!(
        (image_y - 72.0).abs() <= 0.15,
        "page-top anchor must start at the content top, got {image_y}"
    );
}

#[test]
fn keep_next_group_does_not_swallow_a_forced_page_break() {
    let body = r#"<w:p><w:pPr><w:keepNext/></w:pPr><w:r><w:t>Kept heading</w:t></w:r></w:p><w:p><w:pPr><w:pageBreakBefore/></w:pPr><w:r><w:t>Forced next page</w:t></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(laid.pages.len(), 2);
    assert!(page_text(&laid.pages[0]).contains("Kept heading"));
    assert!(page_text(&laid.pages[1]).contains("Forced next page"));
}

#[test]
fn style_backed_bottom_paragraph_border_is_painted_and_measured() {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Bordered title</w:t></w:r></w:p><w:p><w:r><w:t>Following text</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles{DOC_NAMESPACES}><w:style w:type="paragraph" w:styleId="Title"><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="8" w:space="4" w:color="4F81BD"/></w:pBdr><w:ind w:left="360" w:right="360"/></w:pPr></w:style></w:styles>"#
    );
    let bytes = package_with_parts(body, rels, vec![("word/styles.xml", styles.into_bytes())]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let (line_x, line_y, line_dx, line_width, line_color) = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Line { dx_pt, stroke, .. } => Some((
                positioned.x_pt,
                positioned.y_pt,
                *dx_pt,
                stroke.width_pt,
                (stroke.color.r, stroke.color.g, stroke.color.b),
            )),
            _ => None,
        })
        .expect("bottom paragraph border");
    assert!(
        (line_x - 88.5).abs() <= 0.01,
        "the border must honor the 18pt paragraph indent, then include Word's \
         1.5pt border-edge overhang: line_x={line_x}"
    );
    assert!(
        (line_dx - 435.0).abs() <= 0.01,
        "the border must include the edge overhang at both ends: line_dx={line_dx}"
    );
    assert!((line_width - 1.0).abs() <= 0.01, "line_width={line_width}");
    assert_eq!(line_color, (0x4f, 0x81, 0xbd));
    let following_y = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| {
            matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
        })
        .nth(1)
        .expect("following text");
    assert!(
        following_y > line_y,
        "border must contribute to paragraph height"
    );
}

#[test]
fn fractional_table_grid_uses_one_cumulative_edge_for_spanned_cells_and_borders() {
    // 5924 twips is deliberately not a whole-point total. A spanned cell must
    // end on the same cumulatively computed boundary as the table border;
    // summing its component widths independently produces a distinct f64.
    let bytes = minimal_docx(
        r#"<w:tbl><w:tblPr><w:tblLayout w:type="fixed"/><w:tblBorders>
        <w:top w:val="single" w:sz="8"/><w:bottom w:val="single" w:sz="8"/>
        <w:left w:val="single" w:sz="8"/><w:right w:val="single" w:sz="8"/>
        <w:insideV w:val="single" w:sz="8"/>
        </w:tblBorders></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2213"/><w:gridCol w:w="1061"/><w:gridCol w:w="2650"/></w:tblGrid>
        <w:tr><w:trPr><w:gridBefore w:val="1"/></w:trPr>
        <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:shd w:val="clear" w:fill="D9E2F3"/></w:tcPr>
        <w:p><w:r><w:t>SPAN</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
    );
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let page = &laid.pages[0];
    let shaded_right = page
        .items
        .iter()
        .find_map(|positioned| match positioned.item {
            Item::Rect {
                w_pt,
                fill: Some(fill),
                ..
            } if (fill.r, fill.g, fill.b) == (0xd9, 0xe2, 0xf3) => Some(positioned.x_pt + w_pt),
            _ => None,
        })
        .expect("spanned cell shading");
    let right_border = page
        .items
        .iter()
        .filter_map(|positioned| match positioned.item {
            Item::Line { dx_pt, dy_pt, .. } if dx_pt == 0.0 && dy_pt > 0.0 => Some(positioned.x_pt),
            _ => None,
        })
        .max_by(f64::total_cmp)
        .expect("right table border");
    assert_eq!(
        shaded_right, right_border,
        "cell boxes and border emission must share one cumulative grid edge"
    );
}

#[test]
fn paragraph_style_run_style_color_survives_line_wrapping() {
    let body = r#"<w:p><w:pPr><w:pStyle w:val="Endpoint"/></w:pPr><w:r><w:t>alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron</w:t></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles{DOC_NAMESPACES}>
        <w:style w:type="character" w:styleId="CodeBase"><w:rPr><w:color w:val="A0A0A0"/></w:rPr></w:style>
        <w:style w:type="character" w:styleId="Code"><w:basedOn w:val="CodeBase"/><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Endpoint"><w:pPr><w:ind w:left="3600" w:right="3600"/></w:pPr><w:rPr><w:rStyle w:val="Code"/></w:rPr></w:style>
        </w:styles>"#
    );
    let bytes = package_with_parts(body, rels, vec![("word/styles.xml", styles.into_bytes())]);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let glyph_runs: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) if !run.glyphs.is_empty() => Some((positioned.y_pt, run)),
            _ => None,
        })
        .collect();
    assert!(
        glyph_runs
            .iter()
            .all(|(_, run)| (run.color.r, run.color.g, run.color.b) == (0xa0, 0xa0, 0xa0)),
        "every physical fragment must retain the inherited character-style color"
    );
    let mut baselines: Vec<_> = glyph_runs.iter().map(|(baseline, _)| *baseline).collect();
    baselines.sort_by(f64::total_cmp);
    baselines.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    assert!(
        baselines.len() >= 2,
        "fixture must wrap so continuation-line color is exercised: {baselines:?}"
    );
}

#[test]
fn single_right_paragraph_border_is_supported_and_painted() {
    let body = r#"<w:p><w:pPr><w:pBdr><w:right w:val="single" w:sz="4" w:space="4" w:color="112233"/></w:pBdr></w:pPr><w:r><w:t>Right bordered</w:t></w:r></w:p><w:p><w:r><w:t>Following</w:t></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    let predicted = profile(&bytes, OfficeFormat::Docx).expect("profile right border");
    assert!(
        predicted.supported,
        "unexpected reasons: {:?}",
        predicted.reasons
    );
    assert!(convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()).is_ok());

    let document = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&document).unwrap().0;
    let (x, y, dx, dy, width, color) = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match &positioned.item {
            Item::Line {
                dx_pt,
                dy_pt,
                stroke,
            } if dy_pt.abs() > 0.01 => Some((
                positioned.x_pt,
                positioned.y_pt,
                *dx_pt,
                *dy_pt,
                stroke.width_pt,
                (stroke.color.r, stroke.color.g, stroke.color.b),
            )),
            _ => None,
        })
        .expect("right paragraph border");
    assert!((x - 544.25).abs() <= 0.01, "x={x}");
    assert!((y - 72.0).abs() <= 0.01, "y={y}");
    assert!(dx.abs() <= 0.01, "dx={dx}");
    assert!(dy > 10.0, "dy={dy}");
    assert!((width - 0.5).abs() <= 0.01, "width={width}");
    assert_eq!(color, (0x11, 0x22, 0x33));

    let text_end = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => {
                Some(positioned.x_pt + run.glyphs.iter().map(|glyph| glyph.advance_pt).sum::<f64>())
            }
            _ => None,
        })
        .next()
        .expect("bordered text run");
    assert!(
        text_end <= x - width / 2.0 - 4.0 + 0.01,
        "text_end={text_end}, border_x={x}"
    );

    let plain = minimal_docx(
        r#"<w:p><w:r><w:t>Right bordered</w:t></w:r></w:p><w:p><w:r><w:t>Following</w:t></w:r></w:p>"#,
    );
    let plain_doc = WordDoc::open(&plain, &ConvertOptions::default()).unwrap();
    let plain_laid = layout_docx(&plain_doc).unwrap().0;
    let following_baseline = |page: &Page| {
        page.items
            .iter()
            .filter_map(|positioned| {
                matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
            })
            .nth(1)
            .expect("following baseline")
    };
    assert!(
        (following_baseline(&laid.pages[0]) - following_baseline(&plain_laid.pages[0])).abs()
            <= 0.01,
        "a right border must not add vertical height"
    );
}

#[test]
fn right_paragraph_border_spans_the_complete_multiline_box() {
    let text = "multiline paragraph content ".repeat(80);
    let body = format!(
        r#"<w:p><w:pPr><w:pBdr><w:right w:val="single" w:sz="4" w:space="4" w:color="000000"/></w:pBdr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    );
    let bytes = minimal_docx(&body);
    let document = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&document).unwrap().0;
    let page = &laid.pages[0];
    let mut baselines: Vec<f64> = page
        .items
        .iter()
        .filter_map(|positioned| {
            matches!(positioned.item, Item::Glyphs(_)).then_some(positioned.y_pt)
        })
        .collect();
    baselines.sort_by(|left, right| left.total_cmp(right));
    let border_height = page
        .items
        .iter()
        .find_map(|positioned| match positioned.item {
            Item::Line { dx_pt, dy_pt, .. } if dx_pt.abs() <= 0.01 && dy_pt > 0.01 => Some(dy_pt),
            _ => None,
        })
        .expect("right paragraph border");
    let text_span = baselines.last().unwrap() - baselines.first().unwrap();
    assert!(baselines.len() > 2, "expected wrapped lines: {baselines:?}");
    assert!(
        border_height > text_span + 5.0,
        "border={border_height}, text_span={text_span}"
    );
}

#[test]
fn invisible_bar_paragraph_borders_remain_supported() {
    for value in ["nil", "none"] {
        let body = format!(
            r#"<w:p><w:pPr><w:pBdr><w:bar w:val="{value}"/></w:pBdr></w:pPr><w:r><w:t>Invisible bar</w:t></w:r></w:p>"#
        );
        let bytes = minimal_docx(&body);
        assert_profile_convert_agree(&bytes);
    }
}

#[test]
fn visible_bar_paragraph_border_fails_typed() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:pBdr><w:bar w:val="single" w:sz="8" w:space="0" w:color="000000"/></w:pBdr></w:pPr><w:r><w:t>Visible bar</w:t></w:r></w:p>"#,
    );
    assert_profile_convert_reject(&bytes, UnsupportedCode::UnknownElement);
}

#[test]
fn unknown_paragraph_border_child_fails_typed() {
    let bytes = minimal_docx(
        r#"<w:p><w:pPr><w:pBdr><w:diagonal w:val="nil"/></w:pBdr></w:pPr><w:r><w:t>Unknown edge</w:t></w:r></w:p>"#,
    );
    assert_profile_convert_reject(&bytes, UnsupportedCode::UnknownElement);
}

#[test]
fn unknown_attribute_on_visible_paragraph_border_fails_typed() {
    for extra in [
        r#"w:foo="bar""#,
        r#"xmlns:foreign="urn:foreign" foreign:frame="1""#,
    ] {
        let body = format!(
            r#"<w:p><w:pPr><w:pBdr><w:right w:val="single" w:sz="8" w:space="0" w:color="000000" {extra}/></w:pBdr></w:pPr><w:r><w:t>Unknown attribute</w:t></w:r></w:p>"#
        );
        let bytes = minimal_docx(&body);
        assert_profile_convert_reject(&bytes, UnsupportedCode::UnknownElement);
    }
}

#[test]
fn unsupported_paragraph_border_edges_and_styles_fail_typed() {
    for borders in [
        r#"<w:top w:val="single" w:sz="8" w:color="000000"/>"#,
        r#"<w:left w:val="single" w:sz="8" w:color="000000"/>"#,
        r#"<w:between w:val="single" w:sz="8" w:color="000000"/>"#,
        r#"<w:bottom w:val="double" w:sz="8" w:color="000000"/>"#,
        r#"<w:bottom w:val="single" w:sz="bad" w:space="0" w:color="000000"/>"#,
        r#"<w:bottom w:val="single" w:sz="8" w:space="0"/>"#,
        r#"<w:bottom w:val="single" w:sz="8" w:space="0" w:color="000000" w:themeColor="accent1"/>"#,
        r#"<w:bottom w:val="single" w:sz="8" w:space="0" w:color="000000" w:shadow="1"/>"#,
        r#"<w:right w:val="double" w:sz="8" w:space="0" w:color="000000"/>"#,
        r#"<w:right w:val="single" w:sz="bad" w:space="0" w:color="000000"/>"#,
        r#"<w:right w:val="single" w:sz="8" w:space="0"/>"#,
        r#"<w:right w:val="single" w:sz="8" w:space="0" w:color="000000" w:themeColor="accent1"/>"#,
        r#"<w:right w:val="single" w:sz="8" w:space="0" w:color="000000" w:shadow="1"/>"#,
    ] {
        let body = format!(
            r#"<w:p><w:pPr><w:pBdr>{borders}</w:pBdr></w:pPr><w:r><w:t>Bordered paragraph</w:t></w:r></w:p>"#
        );
        let bytes = minimal_docx(&body);
        assert_profile_convert_reject(&bytes, UnsupportedCode::UnknownElement);
    }
}

#[test]
fn visible_docx_images_fail_closed_for_every_unrenderable_resource_state() {
    let drawing = |crop: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">{crop}<a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        )
    };
    let image_rel = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;

    let missing_relationship = package_with_parts(
        &drawing(""),
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>",
        vec![],
    );
    assert_docx_image_reason(&missing_relationship, "missing_relationship_target");

    let missing_part = package_with_parts(&drawing(""), image_rel, vec![]);
    assert_docx_image_reason(&missing_part, "missing_relationship_target");

    let corrupt = package_with_parts(
        &drawing(""),
        image_rel,
        vec![("word/media/image1.png", b"not an image".to_vec())],
    );
    assert_docx_image_reason(&corrupt, "image_decode_failure");

    let disguised_tiff = package_with_parts(
        &drawing(""),
        image_rel,
        vec![("word/media/image1.png", b"II*\0unsupported tiff".to_vec())],
    );
    assert_docx_image_reason(&disguised_tiff, "unsupported_image_encoding");

    let oversized = package_with_parts(
        &drawing(""),
        image_rel,
        vec![("word/media/image1.png", oversized_png_header(10_000, 6_000))],
    );
    assert_docx_image_reason(&oversized, "image_dimensions_exceed_limit");

    let invalid_crop = package_with_parts(
        &drawing(r#"<a:srcRect l="60000" r="60000"/>"#),
        image_rel,
        vec![(
            "word/media/image1.png",
            std::fs::read("../mock-files/png/file1.png").unwrap(),
        )],
    );
    assert_docx_image_reason(&invalid_crop, "invalid_image_crop");
}

#[test]
fn fixture_docx_ole_raster_preview_renders_and_emf_is_typed() {
    let body = r#"<w:p><w:r><w:object><v:shape style="width:72pt;height:36pt"><v:imagedata r:id="preview"/></v:shape><o:OLEObject xmlns:o="urn:schemas-microsoft-com:office:office" r:id="embeddedOle" ProgID="Package"/></w:object></w:r></w:p>"#;
    let png_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="preview" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/ole-preview.png"/><Relationship Id="embeddedOle" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="embeddings/object1.bin"/></Relationships>"#;
    let png = std::fs::read("../mock-files/png/file1.png").unwrap();
    let raster = package_with_parts(
        body,
        png_rels,
        vec![
            ("word/media/ole-preview.png", png),
            ("word/embeddings/object1.bin", b"OLE".to_vec()),
        ],
    );
    assert_profile_convert_agree(&raster);
    let doc = WordDoc::open(&raster, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Image { .. }))
    );
    let corrupt = package_with_parts(
        body,
        png_rels,
        vec![
            ("word/media/ole-preview.png", b"corrupt png".to_vec()),
            ("word/embeddings/object1.bin", b"OLE".to_vec()),
        ],
    );
    assert_docx_image_reason(&corrupt, "image_decode_failure");

    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    let jpeg_rels = png_rels.replace("ole-preview.png", "ole-preview.jpg");
    let jpeg_doc = package_with_parts(
        body,
        &jpeg_rels,
        vec![
            ("word/media/ole-preview.jpg", jpeg),
            ("word/embeddings/object1.bin", b"OLE".to_vec()),
        ],
    );
    assert_profile_convert_agree(&jpeg_doc);

    let emf_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="preview" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/ole-preview.emf"/></Relationships>"#;
    let emf = package_with_parts(
        body,
        emf_rels,
        vec![("word/media/ole-preview.emf", b"EMF preview".to_vec())],
    );
    let prof = profile(&emf, OfficeFormat::Docx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::OlePreviewEmf)
    );
    assert!(matches!(
        convert(&emf, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::OlePreviewEmf,
            ..
        })
    ));

    let wmf_rels = emf_rels.replace("ole-preview.emf", "ole-preview.wmf");
    let wmf = package_with_parts(
        body,
        &wmf_rels,
        vec![("word/media/ole-preview.wmf", b"WMF preview".to_vec())],
    );
    let prof = profile(&wmf, OfficeFormat::Docx).unwrap();
    assert_eq!(
        prof.reasons.first().map(|reason| reason.code),
        Some(UnsupportedCode::OlePreviewWmf)
    );
    assert!(matches!(
        convert(&wmf, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::OlePreviewWmf,
            ..
        })
    ));
}

#[test]
fn fixture_omml_structural_subset_renders_inline_and_centered_display() {
    let body = r#"
    <w:p><w:r><w:t>Inline </w:t></w:r><m:oMath>
      <m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f>
      <m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup>
      <m:sSub><m:e><m:r><m:t>y</m:t></m:r></m:e><m:sub><m:r><m:t>i</m:t></m:r></m:sub></m:sSub>
      <m:sSubSup><m:e><m:r><m:t>z</m:t></m:r></m:e><m:sub><m:r><m:t>j</m:t></m:r></m:sub><m:sup><m:r><m:t>3</m:t></m:r></m:sup></m:sSubSup>
      <m:rad><m:deg><m:r><m:t>3</m:t></m:r></m:deg><m:e><m:r><m:t>q</m:t></m:r></m:e></m:rad>
      <m:nary><m:naryPr><m:chr m:val="∑"/></m:naryPr><m:sub><m:r><m:t>i=0</m:t></m:r></m:sub><m:sup><m:r><m:t>n</m:t></m:r></m:sup><m:e><m:r><m:t>x</m:t></m:r></m:e></m:nary>
      <m:d><m:dPr><m:begChr m:val="("/><m:endChr m:val=")"/></m:dPr><m:e><m:r><m:t>u+v</m:t></m:r></m:e></m:d>
      <m:func><m:fName><m:r><m:t>sin</m:t></m:r></m:fName><m:e><m:r><m:t>t</m:t></m:r></m:e></m:func>
    </m:oMath></w:p>
    <m:oMathPara><m:oMath><m:nary><m:naryPr><m:chr m:val="∫"/></m:naryPr><m:sub><m:r><m:t>0</m:t></m:r></m:sub><m:sup><m:r><m:t>1</m:t></m:r></m:sup><m:e><m:rad><m:e><m:r><m:t>x</m:t></m:r></m:e></m:rad></m:e></m:nary></m:oMath></m:oMathPara>"#;
    let bytes = minimal_docx(body);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let all_text = laid.pages.iter().map(page_text).collect::<String>();
    for expected in ["a", "b", "x", "2", "y", "i", "z", "j", "√", "∑", "∫", "sin"] {
        assert!(
            all_text.contains(expected),
            "missing {expected:?} in {all_text:?}"
        );
    }
    assert!(laid.pages.iter().flat_map(|page| &page.items).any(|positioned| {
        matches!(&positioned.item, Item::Glyphs(run) if run.glyphs.iter().any(|glyph| glyph.ch == 'x') && doc.fonts.face(run.face).is_some_and(|face| face.italic))
    }), "math variables did not use italic shaping");
    let integral_x = laid.pages.iter().flat_map(|page| &page.items).find_map(|positioned| {
        matches!(&positioned.item, Item::Glyphs(run) if run.glyphs.iter().any(|glyph| glyph.ch == '∫')).then_some(positioned.x_pt)
    }).expect("display integral");
    assert!(
        integral_x > 200.0,
        "display equation was not centered: x={integral_x}"
    );
}

#[test]
fn fixture_omml_delimiters_preserve_complete_semantic_graphemes() {
    let body = r#"<w:p><m:oMath><m:d><m:dPr><m:begChr m:val="é"/><m:endChr m:val="⌚️"/></m:dPr><m:e><m:r><m:t>x</m:t></m:r></m:e></m:d></m:oMath></w:p>"#;
    let bytes = minimal_docx(body);
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..ConvertOptions::default()
    };
    let prof = profile_with_options(&bytes, OfficeFormat::Docx, &options).unwrap();
    assert!(prof.supported, "{:?}", prof.reasons);
    let doc = WordDoc::open(&bytes, &options).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let extracted: String = page_text(&laid.pages[0])
        .chars()
        .filter(|character| *character != '\0')
        .collect();
    assert!(extracted.contains("éx⌚️"), "{extracted:?}");
}

#[test]
fn fixture_omml_matrix_array_and_accent_remain_typed() {
    for (xml, detail) in [
        (
            r#"<m:m><m:mr><m:e><m:r><m:t>x</m:t></m:r></m:e></m:mr></m:m>"#,
            "matrix",
        ),
        (
            r#"<m:eqArr><m:e><m:r><m:t>x</m:t></m:r></m:e></m:eqArr>"#,
            "equation array",
        ),
        (
            r#"<m:acc><m:e><m:r><m:t>x</m:t></m:r></m:e></m:acc>"#,
            "accent",
        ),
        (
            r#"<m:d><m:e><m:r><m:t>x</m:t></m:r></m:e><m:e><m:r><m:t>y</m:t></m:r></m:e></m:d>"#,
            "delimiter with multiple expressions",
        ),
    ] {
        let bytes = minimal_docx(&format!("<w:p><m:oMath>{xml}</m:oMath></w:p>"));
        let prof = profile(&bytes, OfficeFormat::Docx).unwrap();
        assert!(!prof.supported);
        assert!(
            prof.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::Equation && reason.detail.contains(detail)
            }),
            "{detail}: {:?}",
            prof.reasons
        );
        assert!(matches!(
            convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
            Err(office_core::OfficeError::Unsupported {
                code: UnsupportedCode::Equation,
                ..
            })
        ));
    }

    let mut nested = "<m:r><m:t>x</m:t></m:r>".to_string();
    for _ in 0..40 {
        nested =
            format!("<m:sSup><m:e>{nested}</m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup>");
    }
    let bytes = minimal_docx(&format!("<w:p><m:oMath>{nested}</m:oMath></w:p>"));
    let prof = profile(&bytes, OfficeFormat::Docx).unwrap();
    assert!(prof.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::Equation && reason.detail.contains("nesting exceeds limit")
    }));
    assert!(matches!(
        convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::Equation,
            ..
        })
    ));
}

#[test]
fn fixture_docx_cached_chart_call_site_renders_geometry_and_labels() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="3657600" cy="1828800"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="chart"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="chart" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="custom/chart-data.xml"/><Relationship Id="orphan" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="charts/orphan.xml"/></Relationships>"#;
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            (
                "word/custom/chart-data.xml",
                cached_chart_xml().as_bytes().to_vec(),
            ),
            (
                "word/charts/orphan.xml",
                br#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:radarChart/></c:plotArea></c:chart></c:chartSpace>"#.to_vec(),
            ),
        ],
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|item| matches!(item.item, Item::Rect { fill: Some(_), .. }))
    );
    assert!(page_text(&laid.pages[0]).contains("11"));
}

#[test]
fn fixture_docx_smartart_uses_dsp_drawing_fallback() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="3657600" cy="1828800"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="diagramData" r:lo="" r:qs="" r:cs=""/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="diagramData" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="diagrams/data1.xml"/></Relationships>"#;
    let data = br#"<?xml version="1.0"?><dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram"><dgm:ptLst/><dgm:cxnLst/><dgm:extLst><a:ext xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:dataModelExt relId="fallback"/></a:ext></dgm:extLst></dgm:dataModel>"#.to_vec();
    let data_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="fallback" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="drawing1.xml"/></Relationships>"#.to_vec();
    let drawing = br#"<?xml version="1.0"?><dsp:drawing xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:spTree><dsp:sp><dsp:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3657600" cy="1828800"/></a:xfrm><a:solidFill><a:srgbClr val="AA3377"/></a:solidFill><a:prstGeom prst="roundRect"/></dsp:spPr><dsp:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Resolved SmartArt</a:t></a:r></a:p></dsp:txBody></dsp:sp></dsp:spTree></dsp:drawing>"#.to_vec();
    let bytes = package_with_parts(
        body,
        rels,
        vec![
            ("word/diagrams/data1.xml", data),
            ("word/diagrams/_rels/data1.xml.rels", data_rels),
            ("word/diagrams/drawing1.xml", drawing),
        ],
    );
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(laid.pages[0].items.iter().any(|item| matches!(&item.item, Item::Path { paint, .. } if paint.fill.is_some_and(|color| (color.r, color.g, color.b) == (170, 51, 119)))));
    assert!(page_text(&laid.pages[0]).contains("Resolved SmartArt"));
}

#[test]
fn fixture_floating_table_positions_and_agrees() {
    let body = r#"<w:tbl><w:tblPr><w:tblpPr w:tblpX="1440" w:tblpY="720" w:horzAnchor="margin" w:vertAnchor="paragraph" w:leftFromText="180"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Floating cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Following body</w:t></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("Floating cell") && text.contains("Following body"));
}

#[test]
fn fixture_vml_rect_textbox_renders_and_agrees() {
    let body = r##"<w:p><w:r><w:pict><v:rect style="position:absolute;left:36pt;top:18pt;width:144pt;height:72pt" fillcolor="#DDEBF7" strokecolor="#4472C4"><v:textbox><w:txbxContent><w:p><w:r><w:t>VML box</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect></w:pict></w:r></w:p>"##;
    let bytes = minimal_docx(body);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert!(page_text(&laid.pages[0]).contains("VML box"));
}

#[test]
fn fixture_real_wordart_warp_is_stably_typed() {
    let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p><w:r><w:t>Warped</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr><a:prstTxWarp prst="textArchUp"/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let bytes = minimal_docx(body);
    let predicted = profile(&bytes, OfficeFormat::Docx).unwrap();
    assert!(predicted.reasons.iter().any(|reason| reason.code
        == office_core::UnsupportedCode::WordArt
        && reason.detail.contains("wordart-warp")));
}

#[test]
fn fixture_section_page_format_border_vertical_align_and_even_start() {
    let body = r#"<w:p><w:r><w:t>Centered body</w:t></w:r></w:p><w:sectPr><w:type w:val="evenPage"/><w:footerReference w:type="default" r:id="rIdF"/><w:pgNumType w:start="1" w:fmt="upperRoman"/><w:vAlign w:val="center"/><w:pgBorders><w:top w:val="single" w:sz="8" w:color="000000"/><w:bottom w:val="single" w:sz="8" w:color="000000"/><w:left w:val="single" w:sz="8" w:color="000000"/><w:right w:val="single" w:sz="8" w:color="000000"/></w:pgBorders></w:sectPr>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdF" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;
    let footer = format!(
        r#"<?xml version="1.0"?><w:ftr{DOC_NAMESPACES}><w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p></w:ftr>"#
    );
    let bytes = package_with_parts(body, rels, vec![("word/footer1.xml", footer.into_bytes())]);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    assert_eq!(laid.pages.len(), 1);
    assert!(page_text(&laid.pages[0]).contains('I'));
    assert!(
        laid.pages[0]
            .items
            .iter()
            .filter(|item| matches!(item.item, Item::Line { .. }))
            .count()
            >= 4
    );
    let body_y = laid.pages[0]
        .items
        .iter()
        .find(|item| {
            matches!(item.item, Item::Glyphs(_))
                && page_text(&Page {
                    width_pt: 0.0,
                    height_pt: 0.0,
                    items: vec![(*item).clone()],
                })
                .contains("Centered")
        })
        .map(|item| item.y_pt)
        .unwrap_or(0.0);
    assert!(body_y > 200.0, "body_y={body_y}");
}

#[test]
fn complex_docx_matches_word_export_page_and_image_geometry() {
    let bytes = std::fs::read("../fidelity-corpus/docx/complex/source.docx")
        .expect("complex.docx fidelity fixture");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "8c2a8c4725e77bbfb4ddc7925674dc221611a39b876dbd7ea2efc829e70b8530",
        "the regression fixture changed; refresh the Word-exported golden measurements first"
    );
    assert_profile_convert_agree(&bytes);

    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).expect("open complex.docx");
    let laid = layout_docx(&doc).expect("layout complex.docx").0;
    assert_eq!(
        laid.pages.len(),
        6,
        "Word exports this fixture as six pages"
    );

    let expected = [
        (90.00, 310.92, 451.40, 300.99),
        (324.00, 148.00, 198.00, 198.00),
        (90.00, 163.40, 143.99, 215.95),
        (72.00, 147.88, 468.29, 170.45),
        (79.95, 163.85, 451.40, 300.99),
        (414.00, 147.98, 108.00, 108.00),
    ];

    for (page_number, (page, expected_bounds)) in laid.pages.iter().zip(expected).enumerate() {
        let images: Vec<(f64, f64, f64, f64)> = page
            .items
            .iter()
            .filter_map(|positioned| match positioned.item {
                Item::Image { w_pt, h_pt, .. } | Item::CroppedImage { w_pt, h_pt, .. } => {
                    Some((positioned.x_pt, positioned.y_pt, w_pt, h_pt))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            images.len(),
            1,
            "page {} must contain exactly one visible image",
            page_number + 1
        );
        let actual = images[0];
        for (label, actual_value, expected_value) in [
            ("x", actual.0, expected_bounds.0),
            ("y", actual.1, expected_bounds.1),
            ("width", actual.2, expected_bounds.2),
            ("height", actual.3, expected_bounds.3),
        ] {
            assert!(
                (actual_value - expected_value).abs() <= 0.15,
                "page {} image {label}: expected {expected_value:.2}pt, got {actual_value:.2}pt",
                page_number + 1
            );
        }
    }

    // On page 3, Word keeps the line whose baseline is near the image bottom
    // beside the tight-wrapped image, then returns the next line to the full
    // content width once its text clears the bottom wrap distance. Testing the
    // whole line box instead of the text band incorrectly constrains one extra
    // line at x=243pt.
    let tight_wrap_line_x = |baseline_min: f64, baseline_max: f64| {
        laid.pages[2]
            .items
            .iter()
            .filter_map(|positioned| {
                matches!(positioned.item, Item::Glyphs(_))
                    .then_some((positioned.x_pt, positioned.y_pt))
            })
            .filter(|(_, baseline)| *baseline >= baseline_min && *baseline <= baseline_max)
            .map(|(x, _)| x)
            .fold(f64::INFINITY, f64::min)
    };
    let last_wrapped_x = tight_wrap_line_x(378.0, 381.0);
    assert!(
        (last_wrapped_x - 243.0).abs() <= 0.15,
        "page 3 last tight-wrapped line: expected x=243pt, got {last_wrapped_x:.2}pt"
    );
    let first_released_x = tight_wrap_line_x(393.0, 396.0);
    assert!(
        (first_released_x - 90.0).abs() <= 0.15,
        "page 3 first line below tight wrap: expected x=90pt, got {first_released_x:.2}pt"
    );

    let table_lines: Vec<(f64, f64, f64, f64)> = laid.pages[5]
        .items
        .iter()
        .filter_map(|positioned| match positioned.item {
            Item::Line { dx_pt, dy_pt, .. } => Some((
                positioned.x_pt,
                positioned.y_pt,
                positioned.x_pt + dx_pt,
                positioned.y_pt + dy_pt,
            )),
            _ => None,
        })
        .collect();
    assert!(!table_lines.is_empty(), "page 6 summary table borders");
    let min_x = table_lines
        .iter()
        .map(|line| line.0.min(line.2))
        .fold(f64::INFINITY, f64::min);
    let max_x = table_lines
        .iter()
        .map(|line| line.0.max(line.2))
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = table_lines
        .iter()
        .map(|line| line.1.min(line.3))
        .fold(f64::INFINITY, f64::min);
    let max_y = table_lines
        .iter()
        .map(|line| line.1.max(line.3))
        .fold(f64::NEG_INFINITY, f64::max);
    for (label, actual, expected, tolerance) in [
        ("left", min_x, 84.62, 0.15),
        ("top", min_y, 432.07, 0.4),
        ("right", max_x, 516.70, 0.15),
        ("bottom", max_y, 628.30, 0.4),
        ("height", max_y - min_y, 196.23, 0.15),
    ] {
        assert!(
            (actual - expected).abs() <= tolerance,
            "page 6 table {label}: expected {expected:.2}pt (+/- {tolerance:.2}), got {actual:.2}pt"
        );
    }
    let after_table_baseline = laid.pages[5]
        .items
        .iter()
        .filter(|positioned| matches!(positioned.item, Item::Glyphs(_)))
        .map(|positioned| positioned.y_pt)
        .find(|baseline| *baseline >= 663.0 && *baseline <= 666.0)
        .expect("page 6 paragraph after summary table");
    assert!(
        (after_table_baseline - 664.66).abs() <= 0.4,
        "page 6 paragraph after table: expected baseline 664.66pt, got {after_table_baseline:.2}pt"
    );

    let behind_page = &laid.pages[3];
    let behind_image_index = behind_page
        .items
        .iter()
        .position(|positioned| {
            matches!(
                positioned.item,
                Item::Image { .. } | Item::CroppedImage { .. }
            )
        })
        .expect("behind-text image");
    let first_text_index = behind_page
        .items
        .iter()
        .position(|positioned| matches!(positioned.item, Item::Glyphs(_)))
        .expect("page 4 text");
    assert!(
        behind_image_index < first_text_index,
        "behindDoc image must paint below every page-body glyph"
    );

    let front_page = &laid.pages[5];
    let front_image_index = front_page
        .items
        .iter()
        .position(|positioned| {
            matches!(
                positioned.item,
                Item::Image { .. } | Item::CroppedImage { .. }
            )
        })
        .expect("front-of-text image");
    let last_text_index = front_page
        .items
        .iter()
        .rposition(|positioned| matches!(positioned.item, Item::Glyphs(_)))
        .expect("page 6 text");
    assert!(
        front_image_index > last_text_index,
        "front-of-text image must paint above every page-body glyph"
    );
}

#[test]
fn fixture_omml_word_equation_boilerplate_properties_are_accepted() {
    // The exact construct mix Word authors for the binomial theorem: math
    // paragraph justification, empty ctrlPr boilerplate on every properties
    // element, a grown n-ary operator, and a no-bar (binomial) fraction.
    let body = r#"<w:p><m:oMathPara><m:oMathParaPr><m:jc m:val="left"/></m:oMathParaPr><m:oMath><m:sSup><m:sSupPr><m:ctrlPr/></m:sSupPr><m:e><m:d><m:dPr><m:ctrlPr/></m:dPr><m:e><m:r><m:t>x+a</m:t></m:r></m:e></m:d></m:e><m:sup><m:r><m:t>n</m:t></m:r></m:sup></m:sSup><m:r><m:t>=</m:t></m:r><m:nary><m:naryPr><m:chr m:val="∑"/><m:grow m:val="1"/><m:ctrlPr/></m:naryPr><m:sub><m:r><m:t>k=0</m:t></m:r></m:sub><m:sup><m:r><m:t>n</m:t></m:r></m:sup><m:e><m:d><m:dPr><m:ctrlPr/></m:dPr><m:e><m:f><m:fPr><m:type m:val="noBar"/><m:ctrlPr/></m:fPr><m:num><m:r><m:t>n</m:t></m:r></m:num><m:den><m:r><m:t>k</m:t></m:r></m:den></m:f></m:e></m:d><m:sSup><m:sSupPr><m:ctrlPr/></m:sSupPr><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>k</m:t></m:r></m:sup></m:sSup><m:sSup><m:sSupPr><m:ctrlPr/></m:sSupPr><m:e><m:r><m:t>a</m:t></m:r></m:e><m:sup><m:r><m:t>n-k</m:t></m:r></m:sup></m:sSup></m:e></m:nary></m:oMath></m:oMathPara></w:p>"#;
    let bytes = minimal_docx(body);
    assert_profile_convert_agree(&bytes);
    let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let all_text = laid.pages.iter().map(page_text).collect::<String>();
    for expected in ["x+a", "=", "∑", "k=0", "n-k"] {
        assert!(
            all_text.contains(expected),
            "missing {expected:?} in {all_text:?}"
        );
    }
}

#[test]
fn fixture_omml_nobar_fraction_suppresses_the_rule_but_keeps_the_stack() {
    let count_lines = |bytes: &[u8]| {
        let doc = WordDoc::open(bytes, &ConvertOptions::default()).unwrap();
        let laid = layout_docx(&doc).unwrap().0;
        laid.pages
            .iter()
            .flat_map(|page| &page.items)
            .filter(|positioned| matches!(positioned.item, Item::Line { .. }))
            .count()
    };
    let barred = minimal_docx(
        r#"<w:p><m:oMath><m:f><m:num><m:r><m:t>n</m:t></m:r></m:num><m:den><m:r><m:t>k</m:t></m:r></m:den></m:f></m:oMath></w:p>"#,
    );
    let binomial = minimal_docx(
        r#"<w:p><m:oMath><m:f><m:fPr><m:type m:val="noBar"/><m:ctrlPr/></m:fPr><m:num><m:r><m:t>n</m:t></m:r></m:num><m:den><m:r><m:t>k</m:t></m:r></m:den></m:f></m:oMath></w:p>"#,
    );
    assert_eq!(count_lines(&barred), 1, "plain fraction draws its rule");
    assert_eq!(
        count_lines(&binomial),
        0,
        "noBar fraction must not draw a rule"
    );
    let doc = WordDoc::open(&binomial, &ConvertOptions::default()).unwrap();
    let laid = layout_docx(&doc).unwrap().0;
    let text = laid.pages.iter().map(page_text).collect::<String>();
    assert!(text.contains('n') && text.contains('k'), "{text:?}");
}

#[test]
fn fixture_omml_display_justification_positions_the_equation() {
    let integral_x = |body: &str| {
        let bytes = minimal_docx(body);
        assert_profile_convert_agree(&bytes);
        let doc = WordDoc::open(&bytes, &ConvertOptions::default()).unwrap();
        let laid = layout_docx(&doc).unwrap().0;
        laid.pages
            .iter()
            .flat_map(|page| &page.items)
            .find_map(|positioned| {
                matches!(&positioned.item, Item::Glyphs(run) if run.glyphs.iter().any(|glyph| glyph.ch == '∫'))
                    .then_some(positioned.x_pt)
            })
            .expect("display integral")
    };
    let equation = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∫"/></m:naryPr><m:sub><m:r><m:t>0</m:t></m:r></m:sub><m:sup><m:r><m:t>1</m:t></m:r></m:sup><m:e><m:r><m:t>x</m:t></m:r></m:e></m:nary></m:oMath>"#;
    let jc = |value: &str| {
        format!(
            "<m:oMathPara><m:oMathParaPr><m:jc m:val=\"{value}\"/></m:oMathParaPr>{equation}</m:oMathPara>"
        )
    };

    // Block-level math paragraphs honor m:jc (default stays centered).
    let left_x = integral_x(&jc("left"));
    let right_x = integral_x(&jc("right"));
    let center_x = integral_x(&jc("centerGroup"));
    assert!(left_x < 150.0, "left-justified equation at x={left_x}");
    assert!(right_x > 420.0, "right-justified equation at x={right_x}");
    assert!(
        (200.0..420.0).contains(&center_x),
        "centerGroup equation at x={center_x}"
    );

    // A display equation wrapped in its own w:p honors m:jc the same way,
    // even when the paragraph style says otherwise.
    let wrapped = format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>{}</w:p>",
        jc("left")
    );
    let wrapped_left_x = integral_x(&wrapped);
    assert!(
        wrapped_left_x < 150.0,
        "w:p-wrapped left-justified equation at x={wrapped_left_x}"
    );
}

#[test]
fn fixture_omml_unrecognized_equation_properties_remain_fail_closed() {
    for (body, detail) in [
        (
            r#"<m:oMathPara><m:oMathParaPr><m:jc m:val="diagonal"/></m:oMathParaPr><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath></m:oMathPara>"#,
            "oMathParaPr",
        ),
        (
            r#"<m:oMathPara><m:oMathParaPr><m:mysteryPr/></m:oMathParaPr><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath></m:oMathPara>"#,
            "oMathParaPr",
        ),
        (
            r#"<w:p><m:oMath><m:f><m:fPr><m:type m:val="lin"/></m:fPr><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f></m:oMath></w:p>"#,
            "fraction type lin",
        ),
        (
            r#"<w:p><m:oMath><m:nary><m:naryPr><m:limLoc m:val="subSup"/></m:naryPr><m:e><m:r><m:t>x</m:t></m:r></m:e></m:nary></m:oMath></w:p>"#,
            "n-ary operator properties",
        ),
        (
            r#"<w:p><m:oMath><m:d><m:dPr><m:sepChr m:val="|"/></m:dPr><m:e><m:r><m:t>x</m:t></m:r></m:e></m:d></m:oMath></w:p>"#,
            "delimiter properties",
        ),
    ] {
        let bytes = minimal_docx(body);
        let prof = profile(&bytes, OfficeFormat::Docx).unwrap();
        assert!(!prof.supported, "{detail}: unexpectedly supported");
        assert!(
            prof.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::Equation && reason.detail.contains(detail)
            }),
            "{detail}: {:?}",
            prof.reasons
        );
        assert!(matches!(
            convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
            Err(office_core::OfficeError::Unsupported {
                code: UnsupportedCode::Equation,
                ..
            })
        ));
    }
}

#[test]
fn fixture_word_default_math_preferences_stay_in_envelope() {
    let empty_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
    let body = r#"<w:p><m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f></m:oMath></w:p>"#;
    let settings = |math_prefs: &str| {
        format!(
            r#"<?xml version="1.0"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:mathPr>{math_prefs}</m:mathPr></w:settings>"#
        )
    };

    // The full block Word stamps into every fresh document with an equation.
    let word_defaults = settings(
        r#"<m:mathFont m:val="Cambria Math"/><m:brkBin m:val="before"/><m:brkBinSub m:val="--"/><m:smallFrac m:val="0"/><m:dispDef/><m:lMargin m:val="0"/><m:rMargin m:val="0"/><m:defJc m:val="centerGroup"/><m:wrapIndent m:val="1440"/><m:intLim m:val="subSup"/><m:naryLim m:val="undOvr"/>"#,
    );
    let bytes = package_with_parts(
        body,
        empty_rels,
        vec![("word/settings.xml", word_defaults.into_bytes())],
    );
    assert_profile_convert_agree(&bytes);

    // Preferences that would visibly move output we cannot produce stay out
    // of envelope when the document actually contains math.
    for non_default in [
        r#"<m:naryLim m:val="subSup"/>"#,
        r#"<m:smallFrac m:val="1"/>"#,
        r#"<m:defJc m:val="left"/>"#,
        r#"<m:lMargin m:val="720"/>"#,
        r#"<m:mathFont m:val="XITS Math"/>"#,
    ] {
        let bytes = package_with_parts(
            body,
            empty_rels,
            vec![("word/settings.xml", settings(non_default).into_bytes())],
        );
        let prof = profile(&bytes, OfficeFormat::Docx).unwrap();
        assert!(!prof.supported, "{non_default}: unexpectedly supported");
        assert!(
            prof.reasons.iter().any(|reason| {
                reason.code == UnsupportedCode::Equation && reason.detail.contains("m:mathPr")
            }),
            "{non_default}: {:?}",
            prof.reasons
        );
        assert!(matches!(
            convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default()),
            Err(office_core::OfficeError::Unsupported {
                code: UnsupportedCode::Equation,
                ..
            })
        ));
    }
}
