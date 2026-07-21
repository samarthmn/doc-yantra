//! PPTX conversion integration tests (agent task 5).

use office_core::layout::{Item, LinkTarget, Page};
use office_core::pptx::{PptxDoc, render_pptx};
use office_core::testkit::zip_package;
use office_core::{
    ConvertOptions, OfficeFormat, UnsupportedCode, convert, profile, profile_with_options,
};

fn opts() -> ConvertOptions {
    ConvertOptions::default()
}

/// Builds a minimal one-slide `.pptx` whose slide's shape tree is `shapes`.
fn minimal_pptx(shapes: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;
    let pres = r#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>"#;
    let pres_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;
    let slide = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
<p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("ppt/presentation.xml", pres.as_bytes()),
        ("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes()),
        ("ppt/slides/slide1.xml", slide.as_bytes()),
    ])
}

fn minimal_pptx_with_slide_rels(shapes: &str, slide_rels: &str) -> Vec<u8> {
    let mut entries = vec![
        (
            "[Content_Types].xml",
            br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>"#.as_slice(),
        ),
        (
            "_rels/.rels",
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#.as_slice(),
        ),
        (
            "ppt/presentation.xml",
            br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>"#.as_slice(),
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.as_slice(),
        ),
    ];
    let slide = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    entries.push(("ppt/slides/slide1.xml", slide.as_bytes()));
    entries.push(("ppt/slides/_rels/slide1.xml.rels", slide_rels.as_bytes()));
    zip_package(&entries)
}

fn pptx_with_table_styles(table: &str, table_styles: &str) -> Vec<u8> {
    let frame = format!(
        r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="2743200"/></p:xfrm><a:graphic><a:graphicData uri="table">{table}</a:graphicData></a:graphic></p:graphicFrame>"#
    );
    let base = minimal_pptx(&frame);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .unwrap();
    content_types.1 = String::from_utf8(content_types.1.clone())
        .unwrap()
        .replace(
            "</Types>",
            r#"<Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#,
        )
        .into_bytes();
    entries.push((
        "ppt/tableStyles.xml".to_string(),
        table_styles.as_bytes().to_vec(),
    ));
    entries.push((
        "ppt/theme/theme1.xml".to_string(),
        br#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Table"><a:dk1><a:srgbClr val="101010"/></a:dk1><a:lt1><a:srgbClr val="F0F0F0"/></a:lt1><a:dk2><a:srgbClr val="202020"/></a:dk2><a:lt2><a:srgbClr val="E0E0E0"/></a:lt2><a:accent1><a:srgbClr val="110000"/></a:accent1><a:accent2><a:srgbClr val="220000"/></a:accent2><a:accent3><a:srgbClr val="330000"/></a:accent3><a:accent4><a:srgbClr val="440000"/></a:accent4><a:accent5><a:srgbClr val="550000"/></a:accent5><a:accent6><a:srgbClr val="660000"/></a:accent6><a:hlink><a:srgbClr val="770000"/></a:hlink><a:folHlink><a:srgbClr val="880000"/></a:folHlink></a:clrScheme><a:fontScheme name="Table"><a:majorFont><a:latin typeface="DejaVu Sans"/></a:majorFont><a:minorFont><a:latin typeface="DejaVu Sans"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#.to_vec(),
    ));
    let refs: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn styled_table(style_id: &str) -> String {
    let mut rows = String::new();
    for row in 0..3 {
        let mut cells = String::new();
        for col in 0..3 {
            cells.push_str(&format!(
                r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>R{row}C{col}</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>"#
            ));
        }
        rows.push_str(&format!(r#"<a:tr h="914400">{cells}</a:tr>"#));
    }
    format!(
        r#"<a:tbl><a:tblPr firstRow="1" lastRow="1" firstCol="1" lastCol="1" bandRow="1"><a:tableStyleId>{style_id}</a:tableStyleId></a:tblPr><a:tblGrid><a:gridCol w="914400"/><a:gridCol w="914400"/><a:gridCol w="914400"/></a:tblGrid>{rows}</a:tbl>"#
    )
}

fn cached_chart_pptx(chart_xml: &str) -> Vec<u8> {
    let frame = r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="4572000" cy="2743200"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="chart1"/></a:graphicData></a:graphic></p:graphicFrame>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="chart1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#;
    let base = minimal_pptx_with_slide_rels(frame, rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .expect("content types");
    let xml = String::from_utf8(content_types.1.clone()).unwrap().replace(
        "</Types>",
        r#"<Override PartName="/ppt/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/></Types>"#,
    );
    content_types.1 = xml.into_bytes();
    entries.push((
        "ppt/charts/chart1.xml".to_string(),
        chart_xml.as_bytes().to_vec(),
    ));
    entries.push((
        "ppt/theme/theme1.xml".to_string(),
        br#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Chart Test"><a:accent3><a:srgbClr val="55AA33"/></a:accent3><a:accent4><a:srgbClr val="AA3355"/></a:accent4></a:clrScheme></a:themeElements></a:theme>"#.to_vec(),
    ));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn smartart_pptx(with_fallback: bool) -> Vec<u8> {
    let frame = r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="3657600" cy="1828800"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="diagramData" r:lo="" r:qs="" r:cs=""/></a:graphicData></a:graphic></p:graphicFrame>"#;
    let slide_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="diagramData" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="../diagrams/data1.xml"/></Relationships>"#;
    let base = minimal_pptx_with_slide_rels(frame, slide_rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .expect("content types");
    let mut overrides = r#"<Override PartName="/ppt/diagrams/data1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml"/>"#.to_string();
    if with_fallback {
        overrides.push_str(r#"<Override PartName="/ppt/diagrams/drawing1.xml" ContentType="application/vnd.ms-office.drawingml.diagramDrawing+xml"/>"#);
    }
    let xml = String::from_utf8(content_types.1.clone())
        .unwrap()
        .replace("</Types>", &format!("{overrides}</Types>"));
    content_types.1 = xml.into_bytes();
    let data = if with_fallback {
        r#"<?xml version="1.0"?><dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram"><dgm:ptLst/><dgm:cxnLst/><dgm:extLst><a:ext xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" uri="smartart"><dsp:dataModelExt relId="drawingFallback"/></a:ext></dgm:extLst></dgm:dataModel>"#
    } else {
        r#"<?xml version="1.0"?><dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:ptLst/><dgm:cxnLst/></dgm:dataModel>"#
    };
    entries.push((
        "ppt/diagrams/data1.xml".to_string(),
        data.as_bytes().to_vec(),
    ));
    if with_fallback {
        entries.push(("ppt/diagrams/_rels/data1.xml.rels".to_string(), br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="drawingFallback" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="drawing1.xml"/></Relationships>"#.to_vec()));
        entries.push(("ppt/diagrams/drawing1.xml".to_string(), br#"<?xml version="1.0"?><dsp:drawing xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:spTree><dsp:sp><dsp:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3657600" cy="1828800"/></a:xfrm><a:solidFill><a:srgbClr val="00AA55"/></a:solidFill><a:prstGeom prst="roundRect"/></dsp:spPr></dsp:sp></dsp:spTree></dsp:drawing>"#.to_vec()));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn smartart_text_pptx(source: &str) -> Vec<u8> {
    let base = smartart_pptx(true);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let drawing = entries
        .iter_mut()
        .find(|(name, _)| name.ends_with("/drawing1.xml"))
        .unwrap();
    drawing.1 = String::from_utf8(drawing.1.clone())
        .unwrap()
        .replace(
            "</dsp:sp>",
            &format!(r#"<dsp:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>{source}</a:t></a:r></a:p></dsp:txBody></dsp:sp>"#),
        )
        .into_bytes();
    let refs: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn smartart_picture_fill_pptx(image: Option<&[u8]>) -> Vec<u8> {
    let base = smartart_pptx(true);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .unwrap();
    content_types.1 = String::from_utf8(content_types.1.clone())
        .unwrap()
        .replace(
            "</Types>",
            r#"<Default Extension="png" ContentType="image/png"/></Types>"#,
        )
        .into_bytes();
    let drawing = entries
        .iter_mut()
        .find(|(name, _)| name.ends_with("/drawing1.xml"))
        .unwrap();
    drawing.1 = String::from_utf8(drawing.1.clone())
        .unwrap()
        .replace(
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#,
        )
        .replace(
            r#"<a:solidFill><a:srgbClr val="00AA55"/></a:solidFill>"#,
            r#"<a:blipFill><a:blip r:embed="preview"/><a:stretch><a:fillRect/></a:stretch></a:blipFill>"#,
        )
        .into_bytes();
    entries.push(("ppt/diagrams/_rels/drawing1.xml.rels".to_string(), br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="preview" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/smartart.png"/></Relationships>"#.to_vec()));
    if let Some(image) = image {
        entries.push(("ppt/media/smartart.png".to_string(), image.to_vec()));
    }
    let refs: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn ole_preview_pptx(extension: &str, content_type: &str, preview: &[u8]) -> Vec<u8> {
    let frame = r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/presentationml/2006/ole"><p:oleObj r:id="embeddedOle" progId="Package"><p:embed/><p:pic><p:blipFill><a:blip r:embed="preview"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:prstGeom prst="rect"/></p:spPr></p:pic></p:oleObj></a:graphicData></a:graphic></p:graphicFrame>"#;
    let rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="embeddedOle" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="../embeddings/object1.bin"/><Relationship Id="preview" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/ole-preview.{extension}"/></Relationships>"#
    );
    let base = minimal_pptx_with_slide_rels(frame, &rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .unwrap();
    let xml = String::from_utf8(content_types.1.clone()).unwrap().replace(
        "</Types>",
        &format!(r#"<Default Extension="{extension}" ContentType="{content_type}"/></Types>"#),
    );
    content_types.1 = xml.into_bytes();
    entries.push((
        format!("ppt/media/ole-preview.{extension}"),
        preview.to_vec(),
    ));
    entries.push(("ppt/embeddings/object1.bin".to_string(), b"OLE".to_vec()));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn cropped_picture_pptx() -> Vec<u8> {
    let mut png = Vec::new();
    let image = image::RgbaImage::from_fn(4, 2, |x, _| image::Rgba([x as u8 * 60, 0, 0, 255]));
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let shapes = r#"<p:pic><p:blipFill><a:blip r:embed="image1"/><a:srcRect l="25000" r="50000"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr></p:pic>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    let base = minimal_pptx_with_slide_rels(shapes, rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .expect("content types");
    let xml = String::from_utf8(content_types.1.clone()).unwrap().replace(
        "</Types>",
        r#"<Default Extension="png" ContentType="image/png"/></Types>"#,
    );
    content_types.1 = xml.into_bytes();
    entries.push(("ppt/media/image1.png".to_string(), png));
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn pptx_with_image_resource(shapes: &str, slide_rels: &str, image: Option<&[u8]>) -> Vec<u8> {
    let base = minimal_pptx_with_slide_rels(shapes, slide_rels);
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let content_types = entries
        .iter_mut()
        .find(|(name, _)| name == "[content_types].xml")
        .expect("content types");
    content_types.1 = String::from_utf8(content_types.1.clone())
        .unwrap()
        .replace(
            "</Types>",
            r#"<Default Extension="png" ContentType="image/png"/></Types>"#,
        )
        .into_bytes();
    if let Some(image) = image {
        entries.push(("ppt/media/image1.png".to_string(), image.to_vec()));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn assert_pptx_image_reason(bytes: &[u8], expected_reason: &str) {
    let prof = profile(bytes, OfficeFormat::Pptx).expect("profile image fixture");
    assert!(!prof.supported, "profile accepted {expected_reason}");
    assert!(
        prof.reasons
            .iter()
            .any(|reason| { serde_json::to_value(reason.code).unwrap() == expected_reason }),
        "missing {expected_reason}: {:?}",
        prof.reasons
    );
    let error = convert(bytes, OfficeFormat::Pptx, &opts()).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = error else {
        panic!("conversion did not return typed unsupported: {error:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);

    let doc = PptxDoc::open(bytes, &opts()).expect("open image fixture");
    let direct = render_pptx(&doc).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = direct else {
        panic!("direct PPTX path did not fail typed: {direct:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);
}

fn assert_pptx_reason(bytes: &[u8], expected_reason: &str) {
    let prof = profile(bytes, OfficeFormat::Pptx).expect("profile PPTX fixture");
    assert!(
        !prof.supported,
        "profile silently accepted {expected_reason}"
    );
    assert!(
        prof.reasons
            .iter()
            .any(|reason| serde_json::to_value(reason.code).unwrap() == expected_reason),
        "missing {expected_reason}: {:?}",
        prof.reasons
    );
    let error = convert(bytes, OfficeFormat::Pptx, &opts()).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = error else {
        panic!("conversion did not return typed unsupported: {error:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);

    let doc = PptxDoc::open(bytes, &opts()).expect("open unsupported fill fixture");
    let direct = render_pptx(&doc).unwrap_err();
    let office_core::OfficeError::Unsupported { code, .. } = direct else {
        panic!("direct PPTX path did not fail typed: {direct:?}");
    };
    assert_eq!(serde_json::to_value(code).unwrap(), expected_reason);
}

fn replace_package_part(bytes: &[u8], part_name: &str, from: &str, to: &str) -> Vec<u8> {
    let package = office_core::opc::Package::open(bytes).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    let part = entries
        .iter_mut()
        .find(|(name, _)| name == part_name)
        .unwrap();
    part.1 = String::from_utf8(part.1.clone())
        .unwrap()
        .replace(from, to)
        .into_bytes();
    let refs: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    zip_package(&refs)
}

fn color_map_override_pptx() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slideLayouts/slideLayout2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
    let root_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
    let presentation = br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="slide1"/><p:sldId id="257" r:id="slide2"/></p:sldIdLst><p:sldMasterIdLst><p:sldMasterId id="1" r:id="master1"/></p:sldMasterIdLst></p:presentation>"#;
    let presentation_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="slide1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="slide2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/><Relationship Id="master1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/></Relationships>"#;
    let shape = r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#;
    let slide1 = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree>{shape}</p:spTree></p:cSld></p:sld>"#
    );
    let slide2 = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree>{shape}</p:spTree></p:cSld><p:clrMapOvr><a:overrideClrMapping tx1="accent2"/></p:clrMapOvr></p:sld>"#
    );
    let slide1_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="layout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#;
    let slide2_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="layout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout2.xml"/></Relationships>"#;
    let layout1 = br#"<?xml version="1.0"?><p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree/></p:cSld><p:clrMapOvr><a:overrideClrMapping tx1="lt1"/></p:clrMapOvr></p:sldLayout>"#;
    let layout2 = br#"<?xml version="1.0"?><p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree/></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;
    let layout_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="master" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;
    let master = br#"<?xml version="1.0"?><p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree/></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/></p:sldMaster>"#;
    let master_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="theme" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
    let theme = br#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Map"><a:dk1><a:srgbClr val="CC0000"/></a:dk1><a:lt1><a:srgbClr val="0000CC"/></a:lt1><a:accent2><a:srgbClr val="00CC00"/></a:accent2></a:clrScheme></a:themeElements></a:theme>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", presentation_rels),
        ("ppt/slides/slide1.xml", slide1.as_bytes()),
        ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", slide1_rels),
        ("ppt/slides/_rels/slide2.xml.rels", slide2_rels),
        ("ppt/slidelayouts/slidelayout1.xml", layout1),
        ("ppt/slidelayouts/slidelayout2.xml", layout2),
        ("ppt/slidelayouts/_rels/slidelayout1.xml.rels", layout_rels),
        ("ppt/slidelayouts/_rels/slidelayout2.xml.rels", layout_rels),
        ("ppt/slidemasters/slidemaster1.xml", master),
        ("ppt/slidemasters/_rels/slidemaster1.xml.rels", master_rels),
        ("ppt/theme/theme1.xml", theme),
    ])
}

fn multi_master_pptx() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slideLayouts/slideLayout2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideMasters/slideMaster2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/theme/theme2.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/></Types>"#;
    let root_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="office" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
    let presentation = br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="slide1"/><p:sldId id="257" r:id="slide2"/></p:sldIdLst><p:sldMasterIdLst><p:sldMasterId id="1" r:id="master1"/><p:sldMasterId id="2" r:id="master2"/></p:sldMasterIdLst></p:presentation>"#;
    let presentation_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="slide1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="slide2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/><Relationship Id="master1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/><Relationship Id="master2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster2.xml"/></Relationships>"#;
    let slide_shape = r#"<p:sp><p:spPr><a:xfrm><a:off x="3657600" y="914400"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:solidFill><a:schemeClr val="accent1"/></a:solidFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#;
    let title = r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:latin typeface="+mj-lt"/></a:rPr><a:t>Master</a:t></a:r></a:p></p:txBody></p:sp>"#;
    let table = r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="3657600"/><a:ext cx="1828800" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="table"><a:tbl><a:tblPr><a:tableStyleId>CUSTOM</a:tableStyleId></a:tblPr><a:tblGrid><a:gridCol w="1828800"/></a:tblGrid><a:tr h="914400"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p/></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#;
    let slide1 = format!(
        r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree>{slide_shape}{title}{table}</p:spTree></p:cSld></p:sld>"#
    );
    let slide2 = slide1.replace(">Master<", ">Theme<");
    let slide1_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="layout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#;
    let slide2_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="layout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout2.xml"/></Relationships>"#;
    let layout = |name: &str| {
        format!(
            r#"<?xml version="1.0"?><p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name="{name}"><p:spTree/></p:cSld></p:sldLayout>"#
        )
    };
    let layout1 = layout("Layout One");
    let layout2 = layout("Layout Two");
    let layout1_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="master" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;
    let layout2_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="master" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster2.xml"/></Relationships>"#;
    let master = |x: i64, static_color: &str, background: &str, size: i64, bullet: &str| {
        format!(
            r#"<?xml version="1.0"?><p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:bg><p:bgPr><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></p:bgPr></p:bg><p:spTree><p:sp><p:spPr><a:xfrm><a:off x="5486400" y="914400"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:solidFill><a:schemeClr val="accent2"/></a:solidFill><a:prstGeom prst="rect"/></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="1828800"/><a:ext cx="1828800" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle><a:lvl1pPr><a:buChar char="{bullet}"/></a:lvl1pPr></a:lstStyle></p:txBody></p:sp></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz="{size}"/></a:lvl1pPr></p:titleStyle></p:txStyles><!-- {static_color} {background} --></p:sldMaster>"#
        )
    };
    let master1 = master(914400, "00AA00", "00CCCC", 3000, "•");
    let master2 = master(1828800, "CCCC00", "CC00CC", 5000, "→");
    let master1_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="theme" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
    let master2_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="theme" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme2.xml"/></Relationships>"#;
    let theme = |font: &str, accent1: &str, accent2: &str, accent3: &str, accent4: &str| {
        format!(
            r#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Per Master"><a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:accent1><a:srgbClr val="{accent1}"/></a:accent1><a:accent2><a:srgbClr val="{accent2}"/></a:accent2><a:accent3><a:srgbClr val="{accent3}"/></a:accent3><a:accent4><a:srgbClr val="{accent4}"/></a:accent4></a:clrScheme><a:fontScheme name="Per Master"><a:majorFont><a:latin typeface="{font}"/></a:majorFont><a:minorFont><a:latin typeface="{font}"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#
        )
    };
    let theme1 = theme("Liberation Sans", "CC0000", "00AA00", "00CCCC", "111111");
    let theme2 = theme("Liberation Serif", "0000CC", "CCCC00", "CC00CC", "EEEEEE");
    let table_styles = br#"<?xml version="1.0"?><a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="CUSTOM"><a:tblStyle styleId="CUSTOM" styleName="Custom"><a:wholeTbl><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent4"/></a:solidFill></a:fill></a:tcStyle></a:wholeTbl></a:tblStyle></a:tblStyleLst>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", presentation_rels),
        ("ppt/slides/slide1.xml", slide1.as_bytes()),
        ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", slide1_rels),
        ("ppt/slides/_rels/slide2.xml.rels", slide2_rels),
        ("ppt/slidelayouts/slidelayout1.xml", layout1.as_bytes()),
        ("ppt/slidelayouts/slidelayout2.xml", layout2.as_bytes()),
        ("ppt/slidelayouts/_rels/slidelayout1.xml.rels", layout1_rels),
        ("ppt/slidelayouts/_rels/slidelayout2.xml.rels", layout2_rels),
        ("ppt/slidemasters/slidemaster1.xml", master1.as_bytes()),
        ("ppt/slidemasters/slidemaster2.xml", master2.as_bytes()),
        ("ppt/slidemasters/_rels/slidemaster1.xml.rels", master1_rels),
        ("ppt/slidemasters/_rels/slidemaster2.xml.rels", master2_rels),
        ("ppt/theme/theme1.xml", theme1.as_bytes()),
        ("ppt/theme/theme2.xml", theme2.as_bytes()),
        ("ppt/tablestyles.xml", table_styles),
    ])
}

fn internal_link_pptx() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>"#;
    let root_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
    let presentation = br#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#;
    let presentation_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#;
    let slide1 = br#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:hlinkClick r:id="jump"/></a:rPr><a:t>Next</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    let slide2 = br#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree/></p:cSld></p:sld>"#;
    let slide_rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="jump" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slide2.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("ppt/presentation.xml", presentation),
        ("ppt/_rels/presentation.xml.rels", presentation_rels),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/slide2.xml", slide2),
        ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
    ])
}

fn three_slide_pptx_with_corrupt_second() -> Vec<u8> {
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide3.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;
    let pres = r#"<?xml version="1.0"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId3"/></p:sldIdLst></p:presentation>"#;
    let pres_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide3.xml"/></Relationships>"#;
    let slide = r#"<?xml version="1.0"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree/></p:cSld></p:sld>"#;
    zip_package(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("ppt/presentation.xml", pres.as_bytes()),
        ("ppt/_rels/presentation.xml.rels", pres_rels.as_bytes()),
        ("ppt/slides/slide1.xml", slide.as_bytes()),
        ("ppt/slides/slide2.xml", b"<not-xml"),
        ("ppt/slides/slide3.xml", slide.as_bytes()),
    ])
}

fn page_text(page: &Page) -> String {
    let mut s = String::new();
    for pos in &page.items {
        match &pos.item {
            Item::Glyphs(run) => {
                for glyph in &run.glyphs {
                    if glyph.ch != '\0' {
                        s.push(glyph.ch);
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

fn item_has_rgb(item: &Item, expected: (u8, u8, u8)) -> bool {
    let matches = |color: &office_core::oxml::Rgba| (color.r, color.g, color.b) == expected;
    match item {
        Item::Transform { item, .. } => item_has_rgb(item, expected),
        Item::Glyphs(run) => matches(&run.color),
        Item::Rect { fill, stroke, .. } => {
            fill.as_ref().is_some_and(&matches)
                || stroke.as_ref().is_some_and(|stroke| matches(&stroke.color))
        }
        Item::Line { stroke, .. } => matches(&stroke.color),
        Item::Path { paint, .. } => {
            paint.fill.as_ref().is_some_and(&matches)
                || paint
                    .stroke
                    .as_ref()
                    .is_some_and(|stroke| matches(&stroke.color))
        }
        Item::Clip { items, .. } => items
            .iter()
            .any(|positioned| item_has_rgb(&positioned.item, expected)),
        Item::SemanticText(_)
        | Item::Image { .. }
        | Item::CroppedImage { .. }
        | Item::Link { .. } => false,
    }
}

fn page_has_rgb(page: &Page, expected: (u8, u8, u8)) -> bool {
    page.items
        .iter()
        .any(|positioned| item_has_rgb(&positioned.item, expected))
}

fn page_glyph_runs(page: &Page) -> Vec<(f64, &office_core::layout::GlyphRun)> {
    fn collect<'a>(
        x: f64,
        item: &'a Item,
        runs: &mut Vec<(f64, &'a office_core::layout::GlyphRun)>,
    ) {
        match item {
            Item::Transform { item, .. } => collect(x, item, runs),
            Item::Glyphs(run) => runs.push((x, run)),
            Item::Clip { items, .. } => {
                for child in items {
                    collect(x + child.x_pt, &child.item, runs);
                }
            }
            _ => {}
        }
    }
    let mut runs = Vec::new();
    for positioned in &page.items {
        collect(positioned.x_pt, &positioned.item, &mut runs);
    }
    runs
}

#[test]
fn converts_every_mock_pptx_to_valid_pdf() {
    let dir = std::path::Path::new("../mock-files/pptx");
    let entries = std::fs::read_dir(dir).expect("required mock-files/pptx fixtures");
    let mut checked = 0;
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("read fixture under {}: {error}", dir.display()))
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("pptx") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let out = convert(&bytes, OfficeFormat::Pptx, &opts())
            .unwrap_or_else(|e| panic!("convert {}: {e}", path.display()));
        assert!(out.pdf.starts_with(b"%PDF-"));
        assert!(out.page_count >= 1, "{} had 0 pages", path.display());
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn slide_text_is_rendered() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let doc = PptxDoc::open(&bytes, &opts()).expect("open");
    let (laid, _) = render_pptx(&doc).expect("render");
    assert_eq!(laid.pages.len(), 1);
    let text = page_text(&laid.pages[0]);
    assert!(text.contains("Dummy Document 1"), "title missing: {text:?}");
    assert!(text.contains("first dummy"), "body missing: {text:?}");
}

#[test]
fn pptx_conversion_is_deterministic() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let a = convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap();
    let b = convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap();
    assert_eq!(a.pdf, b.pdf, "pptx output must be deterministic");
}

#[test]
fn pptx_profile_reports_slides() {
    let bytes = std::fs::read("../mock-files/pptx/file1.pptx").unwrap();
    let prof = profile(&bytes, OfficeFormat::Pptx).expect("profile");
    assert_eq!(prof.format, "pptx");
    assert!(prof.supported);
    assert!(prof.stats.slides >= 1);
}

/// Router contract for PPTX: `profile().supported == convert().is_ok()` on every
/// real mock file. `convert_pptx` profiles first and fails closed, so the
/// unsupported→unsupported direction is structural; the risk this guards is a
/// profile *false positive* — a file profile passes that then fails in
/// `PptxDoc::open`/render/emit. DocKosha's engine-first router depends on this.
#[test]
fn pptx_profile_agrees_with_convert_on_mock_corpus() {
    let dir = std::path::Path::new("../mock-files/pptx");
    let entries = std::fs::read_dir(dir).expect("required mock-files/pptx fixtures");
    let mut checked = 0;
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("read fixture under {}: {error}", dir.display()))
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("pptx") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let prof = profile(&bytes, OfficeFormat::Pptx).expect("profile");
        let conv = convert(&bytes, OfficeFormat::Pptx, &opts());
        assert_eq!(
            prof.supported,
            conv.is_ok(),
            "profile/convert verdict mismatch for {}: reasons={:?} err={:?}",
            path.display(),
            prof.reasons,
            conv.err(),
        );
        checked += 1;
    }
    assert!(checked > 0, "no mock pptx files found");
}

#[test]
fn corrupt_second_of_three_slides_names_slide_two() {
    let bytes = three_slide_pptx_with_corrupt_second();
    for error in [
        profile(&bytes, OfficeFormat::Pptx).unwrap_err(),
        convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            office_core::OfficeError::Invalid(detail) if detail.contains("slide 2")
        ));
    }
}

#[test]
fn corrupt_unreferenced_xml_part_is_still_rejected() {
    let base = minimal_pptx("");
    let package = office_core::opc::Package::open(&base).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = package
        .part_names()
        .map(|name| (name.to_string(), package.part(name).unwrap().to_vec()))
        .collect();
    entries.push(("ppt/unused.xml".to_string(), b"<not-xml".to_vec()));
    let references: Vec<_> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let bytes = zip_package(&references);

    for error in [
        profile(&bytes, OfficeFormat::Pptx).unwrap_err(),
        convert(&bytes, OfficeFormat::Pptx, &opts()).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            office_core::OfficeError::Invalid(detail) if detail.contains("ppt/unused.xml")
        ));
    }
}

/// Common geometry renders; unknown presets retain a named fail-closed reason.
#[test]
fn pptx_geometry_profile_accepts_supported_and_names_unknown() {
    let rect = minimal_pptx(r#"<p:sp><p:spPr><a:prstGeom prst="rect"/></p:spPr></p:sp>"#);
    assert!(
        profile(&rect, OfficeFormat::Pptx).unwrap().supported,
        "a rect-only deck must be supported"
    );

    for geom in [
        r#"<a:prstGeom prst="star5"/>"#,
        r#"<a:prstGeom prst="rightArrow"/>"#,
    ] {
        let deck = minimal_pptx(&format!("<p:sp><p:spPr>{geom}</p:spPr></p:sp>"));
        let prof = profile(&deck, OfficeFormat::Pptx).unwrap();
        assert!(
            prof.supported,
            "{geom} should be supported: {:?}",
            prof.reasons
        );
        assert!(convert(&deck, OfficeFormat::Pptx, &opts()).is_ok());
    }

    let unknown =
        minimal_pptx(r#"<p:sp><p:spPr><a:prstGeom prst="cloudThatDoesNotExist"/></p:spPr></p:sp>"#);
    let prof = profile(&unknown, OfficeFormat::Pptx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::ShapeGeometry
                && reason.detail.contains("cloudThatDoesNotExist"))
    );
    assert!(convert(&unknown, OfficeFormat::Pptx, &opts()).is_err());
}

#[test]
fn pptx_auto_numbered_list_emits_sequential_arabic_periods() {
    let paragraphs = (0..3)
        .map(|index| {
            format!(
                r#"<a:p><a:pPr lvl="0"><a:buAutoNum type="arabicPeriod" startAt="1"/></a:pPr><a:r><a:t>item{index}</a:t></a:r></a:p>"#
            )
        })
        .collect::<String>();
    let bytes = minimal_pptx(&format!(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="4572000" cy="2743200"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp>"#
    ));
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for expected in ["1.", "2.", "3."] {
        assert!(text.contains(expected), "missing {expected} in {text:?}");
    }
}

#[test]
fn pptx_unknown_auto_number_format_fails_closed() {
    let bytes = minimal_pptx(
        r#"<p:sp><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:buAutoNum type="romanUcPeriod"/></a:pPr><a:r><a:t>item</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(!prof.supported);
    assert!(prof.reasons.iter().any(|reason| {
        reason.code == UnsupportedCode::UnknownElement
            && reason.detail.contains("buAutoNum:romanUcPeriod")
    }));
}

#[test]
fn pptx_vertical_table_merge_renders_anchor_once() {
    let bytes = minimal_pptx(
        r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="1828800"/></p:xfrm><a:graphic><a:graphicData uri="table"><a:tbl><a:tblGrid><a:gridCol w="1828800"/></a:tblGrid><a:tr h="914400"><a:tc rowSpan="2"><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>ONLY_ONCE</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr><a:tr h="914400"><a:tc vMerge="1"><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>ONLY_ONCE</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    assert_eq!(text.matches("ONLY_ONCE").count(), 1, "{text:?}");
}

#[test]
fn pptx_gradient_fill_renders_multiple_color_bands() {
    let bytes = minimal_pptx(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="914400"/></a:xfrm><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs><a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    fn collect_colors(item: &Item, colors: &mut std::collections::BTreeSet<(u8, u8, u8)>) {
        match item {
            Item::Rect {
                fill: Some(color), ..
            } => {
                colors.insert((color.r, color.g, color.b));
            }
            Item::Clip { items, .. } => {
                for child in items {
                    collect_colors(&child.item, colors);
                }
            }
            Item::Transform { item, .. } => collect_colors(item, colors),
            _ => {}
        }
    }
    let mut colors = std::collections::BTreeSet::new();
    for positioned in &laid.pages[0].items {
        collect_colors(&positioned.item, &mut colors);
    }
    assert!(colors.len() >= 16, "gradient was flattened: {colors:?}");
}

#[test]
fn pptx_word_art_is_a_typed_miss() {
    let bytes = minimal_pptx(
        r#"<p:sp><p:txBody><a:bodyPr><a:prstTxWarp prst="textArchUp"/></a:bodyPr><a:lstStyle/><a:p><a:r><a:t>WordArt</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::WordArt)
    );
}

#[test]
fn nested_group_child_space_and_rotation_compose() {
    let bytes = minimal_pptx(
        r#"<p:grpSp><p:grpSpPr><a:xfrm rot="2700000"><a:off x="914400" y="914400"/><a:ext cx="3657600" cy="1828800"/><a:chOff x="0" y="0"/><a:chExt cx="1828800" cy="914400"/></a:xfrm></p:grpSpPr><p:grpSp><p:grpSpPr><a:xfrm flipH="1"><a:off x="0" y="0"/><a:ext cx="1828800" cy="914400"/><a:chOff x="0" y="0"/><a:chExt cx="1828800" cy="914400"/></a:xfrm></p:grpSpPr><p:sp><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill><a:prstGeom prst="rect"/></p:spPr></p:sp></p:grpSp></p:grpSp>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let mut item = &laid.pages[0].items[0].item;
    let mut depth = 0;
    while let Item::Transform { item: inner, .. } = item {
        depth += 1;
        item = inner;
    }
    assert_eq!(depth, 2, "both nested group transforms must be retained");
}

#[test]
fn pptx_common_preset_and_custom_geometry_render_as_paths() {
    let custom = r#"<a:custGeom><a:gdLst><a:gd name="mid" fmla="*/ w 1 2"/></a:gdLst><a:pathLst><a:path w="100000" h="100000"><a:moveTo><a:pt x="0" y="50000"/></a:moveTo><a:quadBezTo><a:pt x="mid" y="0"/><a:pt x="100000" y="50000"/></a:quadBezTo><a:cubicBezTo><a:pt x="75000" y="100000"/><a:pt x="25000" y="100000"/><a:pt x="0" y="50000"/></a:cubicBezTo><a:close/></a:path></a:pathLst></a:custGeom>"#;
    for geometry in [
        r#"<a:prstGeom prst="ellipse"/>"#,
        r#"<a:prstGeom prst="rightArrow"><a:avLst><a:gd name="adj1" fmla="val 40000"/></a:avLst></a:prstGeom>"#,
        custom,
    ] {
        let bytes = minimal_pptx(&format!(
            r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="1828800"/></a:xfrm><a:solidFill><a:srgbClr val="4472C4"/></a:solidFill>{geometry}</p:spPr></p:sp>"#
        ));
        let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
        assert!(
            prof.supported,
            "geometry remained unsupported: {:?}",
            prof.reasons
        );
        let doc = PptxDoc::open(&bytes, &opts()).unwrap();
        let (laid, _) = render_pptx(&doc).unwrap();
        let path = laid.pages[0]
            .items
            .iter()
            .find_map(|positioned| match &positioned.item {
                Item::Path { commands, .. } => Some(commands),
                _ => None,
            })
            .expect("geometry must render as one native vector path");
        assert!(path.len() >= 3, "path lost geometry: {path:?}");
        if geometry.contains("cubicBezTo") {
            assert!(
                path.iter().any(|command| matches!(
                    command,
                    office_core::layout::PathCommand::CubicTo { .. }
                )),
                "custom cubic was flattened: {path:?}"
            );
        }
        assert!(
            !laid.pages[0].items.iter().any(|positioned| matches!(
                positioned.item,
                Item::Rect { h_pt, .. } if h_pt <= 1.02
            )),
            "shape still uses scanline rectangles"
        );
    }
}

#[test]
fn pptx_pattern_fill_renders_repeating_marks() {
    let bytes = minimal_pptx(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="1828800"/></a:xfrm><a:pattFill prst="diagCross"><a:fgClr><a:srgbClr val="FF0000"/></a:fgClr><a:bgClr><a:srgbClr val="FFFFFF"/></a:bgClr></a:pattFill><a:prstGeom prst="ellipse"/></p:spPr></p:sp>"#,
    );
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(prof.supported, "pattern fill rejected: {:?}", prof.reasons);
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(
        laid.pages[0].items.iter().any(
            |positioned| matches!(&positioned.item, Item::Clip { items, .. } if items.len() > 8)
        ),
        "non-rectangular pattern fill was not clipped to its shape: {:?}",
        laid.pages[0].items
    );
}

#[test]
fn pptx_cached_bar_chart_profiles_and_renders_series() {
    let bytes = cached_chart_pptx(
        r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:barChart><c:barDir val="col"/><c:grouping val="clustered"/><c:ser><c:idx val="0"/><c:spPr><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></c:spPr><c:dPt><c:idx val="1"/><c:spPr><a:solidFill><a:schemeClr val="accent4"/></a:solidFill></c:spPr></c:dPt><c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>North</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:formatCode>0</c:formatCode><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>25</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea><c:legend><c:legendPos val="r"/></c:legend></c:chart></c:chartSpace>"#,
    );
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(prof.supported, "cached chart rejected: {:?}", prof.reasons);
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let accent3 = doc.theme.color("accent3").unwrap();
    let accent4 = doc.theme.color("accent4").unwrap();
    let rendered_colors: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Rect {
                fill: Some(color), ..
            } => Some((color.r, color.g, color.b)),
            _ => None,
        })
        .collect();
    assert!(rendered_colors.contains(&(accent3.r, accent3.g, accent3.b)));
    assert!(rendered_colors.contains(&(accent4.r, accent4.g, accent4.b)));
    assert!(
        laid.pages[0].items.iter().any(|positioned| matches!(
            positioned.item,
            Item::Rect { fill: Some(color), .. } if (color.r, color.g, color.b) != (255, 255, 255)
        )),
        "chart series did not render: {:?}",
        laid.pages[0].items
    );
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn chart_picture_fill_is_typed_and_never_renders_as_an_empty_chart() {
    let chart = r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:ser><c:idx val="0"/><c:spPr><a:blipFill><a:blip r:embed="chartImage"/><a:stretch><a:fillRect/></a:stretch></a:blipFill></c:spPr><c:cat><c:strLit><c:pt idx="0"><c:v>A</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:pt idx="0"><c:v>1</c:v></c:pt></c:numLit></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
    let bytes = cached_chart_pptx(chart);
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::Chart),
        "{:?}",
        prof.reasons
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Pptx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::Chart,
            ..
        })
    ));
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    assert!(matches!(
        render_pptx(&doc),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::Chart,
            ..
        })
    ));
}

#[test]
fn pptx_chart_completions_render_stacks_area_doughnut_and_value_labels() {
    let series = |name: &str, values: &[i32]| {
        let points = values
            .iter()
            .enumerate()
            .map(|(index, value)| format!(r#"<c:pt idx="{index}"><c:v>{value}</c:v></c:pt>"#))
            .collect::<String>();
        format!(
            r#"<c:ser><c:idx val="0"/><c:tx><c:v>{name}</c:v></c:tx><c:cat><c:strLit><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:formatCode>0</c:formatCode>{points}</c:numLit></c:val><c:dLbls><c:showVal val="1"/></c:dLbls></c:ser>"#
        )
    };
    let cases = [
        (
            "stacked",
            format!(
                r#"<c:barChart><c:barDir val="col"/><c:grouping val="stacked"/>{}{}</c:barChart>"#,
                series("S1", &[10, 20]),
                series("S2", &[30, 10])
            ),
        ),
        (
            "percent",
            format!(
                r#"<c:barChart><c:barDir val="col"/><c:grouping val="percentStacked"/>{}{}</c:barChart>"#,
                series("S1", &[10, 20]),
                series("S2", &[30, 20])
            ),
        ),
        (
            "area",
            format!(
                r#"<c:areaChart><c:grouping val="standard"/>{}</c:areaChart>"#,
                series("Area", &[10, 25])
            ),
        ),
        (
            "line",
            format!(
                r#"<c:lineChart><c:grouping val="standard"/><c:marker><c:symbol val="circle"/></c:marker>{}</c:lineChart>"#,
                series("Line", &[10, 25])
            ),
        ),
        (
            "horizontal",
            format!(
                r#"<c:barChart><c:barDir val="bar"/><c:grouping val="clustered"/>{}</c:barChart>"#,
                series("Horizontal", &[10, 25])
            ),
        ),
        (
            "scatter-labels",
            r#"<c:scatterChart><c:scatterStyle val="marker"/><c:ser><c:idx val="0"/><c:tx><c:v>Scatter</c:v></c:tx><c:xVal><c:numLit><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numLit></c:xVal><c:yVal><c:numLit><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>25</c:v></c:pt></c:numLit></c:yVal><c:dLbls><c:showVal val="1"/></c:dLbls></c:ser></c:scatterChart>"#.to_string(),
        ),
        (
            "doughnut",
            format!(
                r#"<c:doughnutChart><c:holeSize val="55"/>{}</c:doughnutChart>"#,
                series("Ring", &[10, 30])
            ),
        ),
    ];
    for (name, family) in cases {
        let legend = if name == "doughnut" {
            r#"<c:legend><c:legendPos val="r"/></c:legend>"#
        } else {
            ""
        };
        let xml = format!(
            r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea>{family}<c:catAx><c:axId val="1"/></c:catAx><c:valAx><c:axId val="2"/></c:valAx></c:plotArea>{legend}</c:chart></c:chartSpace>"#
        );
        let bytes = cached_chart_pptx(&xml);
        let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
        assert!(prof.supported, "{name}: {:?}", prof.reasons);
        let doc = PptxDoc::open(&bytes, &opts()).unwrap();
        let (laid, _) = render_pptx(&doc).unwrap();
        let text = page_text(&laid.pages[0]);
        assert!(text.contains("10"), "{name} value label missing: {text:?}");
        if matches!(name, "area" | "line" | "horizontal" | "doughnut") {
            assert!(
                text.contains('A') && text.contains('B'),
                "{name} category labels missing: {text:?}"
            );
        }
        if name == "percent" {
            assert!(
                text.contains("25%"),
                "percent axis labels missing: {text:?}"
            );
        }
        match name {
            "area" => assert!(laid.pages[0].items.iter().any(|positioned| matches!(&positioned.item, Item::Path { paint, .. } if paint.fill.is_some()))),
            "line" => assert!(laid.pages[0].items.iter().any(|positioned| matches!(&positioned.item, Item::Path { paint, .. } if paint.fill.is_none() && paint.stroke.is_some()))),
            "doughnut" => assert!(laid.pages[0].items.iter().any(|positioned| matches!(&positioned.item, Item::Path { commands, paint } if paint.fill_rule == office_core::layout::FillRule::EvenOdd && commands.iter().filter(|command| matches!(command, office_core::layout::PathCommand::MoveTo { .. })).count() == 1 && commands.iter().filter(|command| matches!(command, office_core::layout::PathCommand::Close)).count() == 1))),
            "horizontal" | "scatter-labels" => assert!(laid.pages[0].items.iter().filter(|positioned| matches!(positioned.item, Item::Rect { fill: Some(_), .. })).count() >= 2),
            _ => assert!(laid.pages[0].items.iter().filter(|positioned| matches!(positioned.item, Item::Rect { fill: Some(_), .. })).count() >= 4),
        }
        assert!(
            convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok(),
            "{name}"
        );
    }
}

#[test]
fn chart_and_smartart_text_use_the_shared_logical_cluster_mapping() {
    let source = "e\u{301} لا שָׁ क्षि ก้ ⌚️ 👍🏽 👩‍💻";
    let chart = format!(
        r#"<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>{source}</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:barChart><c:barDir val="col"/><c:ser><c:idx val="0"/><c:cat><c:strLit><c:pt idx="0"><c:v>A</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:pt idx="0"><c:v>1</c:v></c:pt></c:numLit></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#
    );
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    for (label, bytes) in [
        ("chart", cached_chart_pptx(&chart)),
        ("SmartArt", smartart_text_pptx(source)),
    ] {
        let prof = profile_with_options(&bytes, OfficeFormat::Pptx, &options).unwrap();
        assert!(prof.supported, "{label}: {:?}", prof.reasons);
        let doc = PptxDoc::open(&bytes, &options).unwrap();
        let (laid, _) = render_pptx(&doc).unwrap();
        let extracted: String = page_text(&laid.pages[0])
            .chars()
            .filter(|character| *character != '\0')
            .collect();
        assert!(
            extracted.contains(source),
            "{label} source was not preserved: {extracted:?}"
        );
    }
}

#[test]
fn pptx_smartart_uses_dsp_fallback_and_absence_stays_typed() {
    let bytes = smartart_pptx(true);
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.supported,
        "fallback SmartArt rejected: {:?}",
        prof.reasons
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|positioned| matches!(
        &positioned.item,
        Item::Path { paint, .. } if paint.fill.is_some_and(|color| (color.r, color.g, color.b) == (0, 170, 85))
    )), "SmartArt fallback shape missing: {:?}", laid.pages[0].items);
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());

    let absent = smartart_pptx(false);
    let prof = profile(&absent, OfficeFormat::Pptx).unwrap();
    assert!(!prof.supported);
    assert!(
        prof.reasons.iter().any(|reason| {
            reason.code == UnsupportedCode::SmartArt && reason.detail.contains("fallback")
        }),
        "missing stable SmartArt fallback reason: {:?}",
        prof.reasons
    );
    assert!(matches!(
        convert(&absent, OfficeFormat::Pptx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::SmartArt,
            ..
        })
    ));
}

#[test]
fn smartart_picture_fill_preview_is_rendered_or_fails_with_the_image_reason() {
    let png = std::fs::read("../mock-files/png/file1.png").unwrap();
    let valid = smartart_picture_fill_pptx(Some(&png));
    let prof = profile(&valid, OfficeFormat::Pptx).unwrap();
    assert!(prof.supported, "{:?}", prof.reasons);
    let doc = PptxDoc::open(&valid, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|positioned| matches!(
        &positioned.item,
        Item::Clip { items, .. }
            if items.iter().any(|child| matches!(child.item, Item::Image { .. }))
    )));

    assert_pptx_image_reason(
        &smartart_picture_fill_pptx(None),
        "missing_relationship_target",
    );
    assert_pptx_image_reason(
        &smartart_picture_fill_pptx(Some(b"corrupt png")),
        "image_decode_failure",
    );
}

#[test]
fn pptx_ole_raster_preview_renders_and_emf_is_typed() {
    let png = std::fs::read("../mock-files/png/file1.png").unwrap();
    let raster = ole_preview_pptx("png", "image/png", &png);
    let prof = profile(&raster, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.supported,
        "raster preview rejected: {:?}",
        prof.reasons
    );
    let doc = PptxDoc::open(&raster, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(
        laid.pages[0]
            .items
            .iter()
            .any(|positioned| matches!(positioned.item, Item::Image { .. }))
    );
    assert!(convert(&raster, OfficeFormat::Pptx, &opts()).is_ok());

    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    let jpeg_deck = ole_preview_pptx("jpg", "image/jpeg", &jpeg);
    assert!(profile(&jpeg_deck, OfficeFormat::Pptx).unwrap().supported);
    assert!(convert(&jpeg_deck, OfficeFormat::Pptx, &opts()).is_ok());

    let emf = ole_preview_pptx("emf", "image/x-emf", b"EMF preview");
    let prof = profile(&emf, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::OlePreviewEmf)
    );
    assert!(matches!(
        convert(&emf, OfficeFormat::Pptx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::OlePreviewEmf,
            ..
        })
    ));

    let wmf = ole_preview_pptx("wmf", "image/x-wmf", b"WMF preview");
    let prof = profile(&wmf, OfficeFormat::Pptx).unwrap();
    assert_eq!(
        prof.reasons.first().map(|reason| reason.code),
        Some(UnsupportedCode::OlePreviewWmf)
    );
    assert!(matches!(
        convert(&wmf, OfficeFormat::Pptx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::OlePreviewWmf,
            ..
        })
    ));

    assert_pptx_image_reason(
        &ole_preview_pptx("png", "image/png", b"corrupt png"),
        "image_decode_failure",
    );
}

#[test]
fn pptx_word_art_none_renders_but_real_warp_stays_typed() {
    let none = minimal_pptx(
        r#"<p:sp><p:txBody><a:bodyPr><a:prstTxWarp prst="textNoShape"/></a:bodyPr><a:lstStyle/><a:p><a:r><a:t>plain</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    assert!(profile(&none, OfficeFormat::Pptx).unwrap().supported);
    assert!(convert(&none, OfficeFormat::Pptx, &opts()).is_ok());

    let warped = minimal_pptx(
        r#"<p:sp><p:txBody><a:bodyPr><a:prstTxWarp prst="textWave1"/></a:bodyPr><a:lstStyle/><a:p><a:r><a:t>warped</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    let prof = profile(&warped, OfficeFormat::Pptx).unwrap();
    assert!(prof.reasons.iter().any(
        |reason| reason.code == UnsupportedCode::WordArt && reason.detail.contains("textWave1")
    ));
}

#[test]
fn pptx_cluster_fallback_shapes_cjk_and_arabic() {
    let bytes = minimal_pptx(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="6400800" cy="1828800"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:latin typeface="Arial"/></a:rPr><a:t>你好 مرحبا</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let prof = profile_with_options(&bytes, OfficeFormat::Pptx, &options).unwrap();
    assert!(prof.supported, "fallback profile miss: {:?}", prof.reasons);
    let doc = PptxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let text = page_text(&laid.pages[0]);
    for expected in ['你', '好', 'م', 'ر', 'ح', 'ب', 'ا'] {
        assert!(
            text.contains(expected),
            "missing {expected:?} from {text:?}"
        );
    }
    assert!(convert(&bytes, OfficeFormat::Pptx, &options).is_ok());
}

#[test]
fn pptx_shaped_clusters_keep_the_complete_logical_source_once() {
    let source = "e\u{301} é لا שָׁ क्षि ก้ ⌚️ 👍🏽 👩‍💻 LTR שלום 123";
    let bytes = minimal_pptx(&format!(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="457200" y="457200"/><a:ext cx="8229600" cy="1828800"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:latin typeface="Arial"/></a:rPr><a:t>{source}</a:t></a:r></a:p></p:txBody></p:sp>"#
    ));
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let prof = profile_with_options(&bytes, OfficeFormat::Pptx, &options).unwrap();
    assert!(prof.supported, "{:?}", prof.reasons);
    let doc = PptxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let extracted: String = page_text(&laid.pages[0])
        .chars()
        .filter(|character| *character != '\0')
        .collect();
    assert_eq!(extracted, source);
}

#[test]
fn pptx_complex_run_before_direct_run_keeps_source_order() {
    let source = "e\u{301} Latin";
    let bytes = minimal_pptx(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="457200" y="457200"/><a:ext cx="3657600" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>é</a:t></a:r><a:r><a:t xml:space="preserve"> Latin</a:t></a:r></a:p></p:txBody></p:sp>"#,
    );
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let doc = PptxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();

    assert_eq!(page_text(&laid.pages[0]), source);
    assert!(laid.pages[0].items.iter().any(|positioned| {
        matches!(
            &positioned.item,
            Item::Glyphs(run) if run.base_direction == Some(text_core::Direction::Ltr)
        )
    }));
}

#[test]
fn pptx_wrapped_rtl_semantics_stay_owned_by_each_physical_line() {
    let source = ["مرحبا"; 12].join(" ");
    let bytes = minimal_pptx(&format!(
        r#"<p:sp><p:spPr><a:xfrm><a:off x="457200" y="457200"/><a:ext cx="1828800" cy="3657600"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr lIns="0" rIns="0" tIns="0" bIns="0"/><a:lstStyle/><a:p><a:pPr rtl="1" algn="r"/><a:r><a:rPr sz="1200"><a:latin typeface="Arial"/></a:rPr><a:t>{source}</a:t></a:r></a:p></p:txBody></p:sp>"#
    ));
    let options = ConvertOptions {
        font_dirs: vec![std::path::PathBuf::from("../pdf-core/wasm/pkg/fonts")],
        ..opts()
    };
    let doc = PptxDoc::open(&bytes, &options).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let semantic: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::SemanticText(run) => Some((positioned.y_pt, run)),
            _ => None,
        })
        .collect();

    assert!(
        semantic.len() > 1,
        "wrapped RTL text must have one semantic item per physical line"
    );
    assert!(
        semantic
            .windows(2)
            .all(|lines| (lines[0].0 - lines[1].0).abs() > 0.01),
        "semantic line items must not collapse onto one baseline"
    );
    let extracted: String = semantic
        .iter()
        .flat_map(|(_, run)| &run.clusters)
        .map(|cluster| cluster.source.as_str())
        .collect();
    assert_eq!(extracted, source);
    assert!(semantic.iter().all(|(_, run)| {
        run.base_direction == text_core::Direction::Rtl
            && run
                .clusters
                .iter()
                .map(|cluster| cluster.advance_pt)
                .sum::<f64>()
                > 0.0
    }));
}

#[test]
fn pptx_external_text_hyperlink_becomes_link_item() {
    let shapes = r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:hlinkClick r:id="rIdLink"/></a:rPr><a:t>OpenAI</a:t></a:r></a:p></p:txBody></p:sp>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://openai.com/docs" TargetMode="External"/></Relationships>"#;
    let bytes = minimal_pptx_with_slide_rels(shapes, rels);
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|item| matches!(&item.item, Item::Link { target: LinkTarget::Url(url), .. } if url == "https://openai.com/docs")));
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn pptx_alternate_content_uses_fallback_shape() {
    let bytes = minimal_pptx(
        r#"<mc:AlternateContent><mc:Choice Requires="p14"><p:sp/></mc:Choice><mc:Fallback><p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="2743200" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>FALLBACK_VISIBLE</a:t></a:r></a:p></p:txBody></p:sp></mc:Fallback></mc:AlternateContent>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(page_text(&laid.pages[0]).contains("FALLBACK_VISIBLE"));
    assert!(profile(&bytes, OfficeFormat::Pptx).unwrap().supported);
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn pptx_table_uses_cell_border_width_and_color() {
    let bytes = minimal_pptx(
        r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="table"><a:tbl><a:tblGrid><a:gridCol w="1828800"/></a:tblGrid><a:tr h="914400"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p/></a:txBody><a:tcPr><a:lnT w="38100"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:lnT></a:tcPr></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|positioned| matches!(&positioned.item, Item::Line { stroke, .. } if (stroke.width_pt - 3.0).abs() < 0.01 && (stroke.color.r, stroke.color.g, stroke.color.b) == (255, 0, 0))));
}

#[test]
fn pptx_custom_table_style_resolves_regions_borders_fill_and_text_through_theme() {
    let styles = r#"<?xml version="1.0"?><a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="CUSTOM"><a:tblStyle styleId="CUSTOM" styleName="Custom">
    <a:wholeTbl><a:tcTxStyle><a:fontRef idx="minor"/><a:schemeClr val="lt1"/></a:tcTxStyle><a:tcStyle><a:tcBdr><a:left><a:ln w="25400"><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:ln></a:left><a:right><a:ln w="25400"><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:ln></a:right><a:top><a:ln w="25400"><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:ln></a:top><a:bottom><a:ln w="25400"><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:ln></a:bottom><a:insideH><a:ln w="12700"><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></a:ln></a:insideH><a:insideV><a:ln w="12700"><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></a:ln></a:insideV></a:tcBdr><a:fill><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fill></a:tcStyle></a:wholeTbl>
    <a:band1H><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:fill></a:tcStyle></a:band1H><a:band2H><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></a:fill></a:tcStyle></a:band2H>
    <a:firstRow><a:tcTxStyle b="1"><a:schemeClr val="accent5"/></a:tcTxStyle><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent4"/></a:solidFill></a:fill></a:tcStyle></a:firstRow>
    <a:lastRow><a:tcTxStyle i="1"><a:schemeClr val="hlink"/></a:tcTxStyle><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent5"/></a:solidFill></a:fill></a:tcStyle></a:lastRow>
    <a:firstCol><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="dk1"/></a:solidFill></a:fill></a:tcStyle></a:firstCol><a:lastCol><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:fill></a:tcStyle></a:lastCol>
    <a:nwCell><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent6"/></a:solidFill></a:fill></a:tcStyle></a:nwCell><a:neCell><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="dk2"/></a:solidFill></a:fill></a:tcStyle></a:neCell><a:swCell><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="lt2"/></a:solidFill></a:fill></a:tcStyle></a:swCell><a:seCell><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="folHlink"/></a:solidFill></a:fill></a:tcStyle></a:seCell>
    </a:tblStyle></a:tblStyleLst>"#;
    let bytes = pptx_with_table_styles(&styled_table("CUSTOM"), styles);
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(prof.supported, "{:?}", prof.reasons);
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let fills: std::collections::BTreeMap<(i32, i32), (u8, u8, u8)> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match positioned.item {
            Item::Rect {
                fill: Some(color), ..
            } => Some((
                (
                    positioned.x_pt.round() as i32,
                    positioned.y_pt.round() as i32,
                ),
                (color.r, color.g, color.b),
            )),
            _ => None,
        })
        .collect();
    for (position, expected) in [
        ((72, 72), (0x66, 0, 0)),
        ((144, 72), (0x44, 0, 0)),
        ((216, 72), (0x20, 0x20, 0x20)),
        ((72, 144), (0x10, 0x10, 0x10)),
        ((144, 144), (0x22, 0, 0)),
        ((216, 144), (0xf0, 0xf0, 0xf0)),
        ((72, 216), (0xe0, 0xe0, 0xe0)),
        ((216, 216), (0x88, 0, 0)),
    ] {
        assert_eq!(fills.get(&position), Some(&expected), "fills: {fills:?}");
    }
    assert!(laid.pages[0].items.iter().any(|positioned| matches!(
        &positioned.item,
        Item::Line { stroke, .. }
            if (stroke.width_pt - 2.0).abs() < 0.01
                && (stroke.color.r, stroke.color.g, stroke.color.b) == (0x22, 0, 0)
    )));
    let text_colors: Vec<_> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some((run.color.r, run.color.g, run.color.b)),
            _ => None,
        })
        .collect();
    assert!(text_colors.contains(&(0x55, 0, 0)), "{text_colors:?}");
    assert!(text_colors.contains(&(0x77, 0, 0)), "{text_colors:?}");
    let text_faces: std::collections::BTreeMap<String, _> = laid.pages[0]
        .items
        .iter()
        .filter_map(|positioned| match &positioned.item {
            Item::Glyphs(run) => Some((
                run.glyphs
                    .iter()
                    .map(|glyph| glyph.ch)
                    .filter(|character| *character != '\0')
                    .collect(),
                run.face,
            )),
            _ => None,
        })
        .collect();
    assert_ne!(
        text_faces["R0C1"], text_faces["R1C1"],
        "bold was not inherited"
    );
    assert_ne!(
        text_faces["R2C1"], text_faces["R1C1"],
        "italic was not inherited"
    );
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn pptx_undefined_builtin_table_style_guid_fails_closed() {
    let built_in = "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}";
    let styles = r#"<?xml version="1.0"?><a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="CUSTOM"><a:tblStyle styleId="CUSTOM" styleName="Custom"><a:wholeTbl><a:tcStyle><a:fill><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:fill></a:tcStyle></a:wholeTbl></a:tblStyle></a:tblStyleLst>"#;
    let bytes = pptx_with_table_styles(&styled_table(built_in), styles);
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(!prof.supported, "profile accepted undefined built-in style");
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::TableStyle),
        "{:?}",
        prof.reasons
    );
    assert!(matches!(
        convert(&bytes, OfficeFormat::Pptx, &opts()),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::TableStyle,
            ..
        })
    ));
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    assert!(matches!(
        render_pptx(&doc),
        Err(office_core::OfficeError::Unsupported {
            code: UnsupportedCode::TableStyle,
            ..
        })
    ));
}

#[test]
fn pptx_table_style_default_and_both_band_axes_resolve_exact_regions() {
    let styles = r#"<?xml version="1.0"?><a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="BANDS"><a:tblStyle styleId="BANDS" styleName="Bands"><a:band1H><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:fill></a:tcStyle></a:band1H><a:band2H><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent3"/></a:solidFill></a:fill></a:tcStyle></a:band2H><a:band1V><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent4"/></a:solidFill></a:fill></a:tcStyle></a:band1V><a:band2V><a:tcStyle><a:fill><a:solidFill><a:schemeClr val="accent5"/></a:solidFill></a:fill></a:tcStyle></a:band2V></a:tblStyle></a:tblStyleLst>"#;
    let table = |rows: usize, cols: usize, flag: &str| {
        let grid = r#"<a:gridCol w="914400"/>"#.repeat(cols);
        let cells = (0..cols)
            .map(|col| format!(r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>C{col}</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>"#))
            .collect::<String>();
        let rows = (0..rows)
            .map(|_| format!(r#"<a:tr h="914400">{cells}</a:tr>"#))
            .collect::<String>();
        format!(r#"<a:tbl><a:tblPr {flag}="1"/><a:tblGrid>{grid}</a:tblGrid>{rows}</a:tbl>"#)
    };
    for (table, axis, expected) in [
        (
            table(4, 1, "bandRow"),
            "row",
            vec![
                (72, 72, 0x22),
                (72, 144, 0x33),
                (72, 216, 0x22),
                (72, 288, 0x33),
            ],
        ),
        (
            table(1, 4, "bandCol"),
            "column",
            vec![
                (72, 72, 0x44),
                (144, 72, 0x55),
                (216, 72, 0x44),
                (288, 72, 0x55),
            ],
        ),
    ] {
        let bytes = pptx_with_table_styles(&table, styles);
        let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
        assert!(prof.supported, "{axis}: {:?}", prof.reasons);
        let doc = PptxDoc::open(&bytes, &opts()).unwrap();
        let (laid, _) = render_pptx(&doc).unwrap();
        let fills: std::collections::BTreeMap<_, _> = laid.pages[0]
            .items
            .iter()
            .filter_map(|positioned| match positioned.item {
                Item::Rect {
                    fill: Some(fill), ..
                } => Some((
                    (
                        positioned.x_pt.round() as i32,
                        positioned.y_pt.round() as i32,
                    ),
                    fill.r,
                )),
                _ => None,
            })
            .collect();
        for (x, y, red) in expected {
            assert_eq!(fills.get(&(x, y)), Some(&red), "{axis}: {fills:?}");
        }
    }
}

#[test]
fn pptx_unknown_pattern_and_unsafe_link_fail_typed() {
    let pattern = minimal_pptx(
        r#"<p:sp><p:spPr><a:pattFill prst="notARealPattern"><a:fgClr><a:srgbClr val="000000"/></a:fgClr><a:bgClr><a:srgbClr val="FFFFFF"/></a:bgClr></a:pattFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#,
    );
    let prof = profile(&pattern, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::PatternFill
                && reason.detail.contains("notARealPattern"))
    );
    assert!(convert(&pattern, OfficeFormat::Pptx, &opts()).is_err());

    let shapes = r#"<p:sp><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr><a:hlinkClick r:id="bad"/></a:rPr><a:t>unsafe</a:t></a:r></a:p></p:txBody></p:sp>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="bad" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="javascript:alert(1)" TargetMode="External"/></Relationships>"#;
    let unsafe_link = minimal_pptx_with_slide_rels(shapes, rels);
    let prof = profile(&unsafe_link, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.reasons
            .iter()
            .any(|reason| reason.code == UnsupportedCode::Hyperlink)
    );
    assert!(convert(&unsafe_link, OfficeFormat::Pptx, &opts()).is_err());
}

#[test]
fn pptx_src_rect_creates_and_uses_cropped_image_resource() {
    let bytes = cropped_picture_pptx();
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(
        prof.supported,
        "crop remained typed unsupported: {:?}",
        prof.reasons
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    let image_id = laid.pages[0]
        .items
        .iter()
        .find_map(|positioned| match positioned.item {
            Item::Image { image_id, .. } => Some(image_id),
            _ => None,
        })
        .expect("cropped image item");
    assert!(matches!(
        &laid.images[image_id.0],
        office_core::layout::EncodedImage::Rgba8 {
            width: 1,
            height: 2,
            ..
        }
    ));
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn visible_pptx_pictures_and_picture_fills_fail_closed_when_unrenderable() {
    let picture = |crop: &str| {
        format!(
            r#"<p:pic><p:blipFill><a:blip r:embed="image1"/>{crop}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr></p:pic>"#
        )
    };
    let image_rel = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;

    let missing_relationship = pptx_with_image_resource(
        &picture(""),
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        None,
    );
    assert_pptx_image_reason(&missing_relationship, "missing_relationship_target");

    let missing_part = pptx_with_image_resource(&picture(""), image_rel, None);
    assert_pptx_image_reason(&missing_part, "missing_relationship_target");

    let corrupt = pptx_with_image_resource(&picture(""), image_rel, Some(b"not an image"));
    assert_pptx_image_reason(&corrupt, "image_decode_failure");

    let disguised_tiff =
        pptx_with_image_resource(&picture(""), image_rel, Some(b"II*\0unsupported tiff"));
    assert_pptx_image_reason(&disguised_tiff, "unsupported_image_encoding");

    let invalid_crop = pptx_with_image_resource(
        &picture(r#"<a:srcRect l="60000" r="60000"/>"#),
        image_rel,
        Some(&std::fs::read("../mock-files/png/file1.png").unwrap()),
    );
    assert_pptx_image_reason(&invalid_crop, "invalid_image_crop");

    let picture_fill = r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:blipFill><a:blip r:embed="image1"/><a:stretch><a:fillRect/></a:stretch></a:blipFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#;
    let corrupt_fill = pptx_with_image_resource(picture_fill, image_rel, Some(b"not an image"));
    assert_pptx_image_reason(&corrupt_fill, "image_decode_failure");
}

#[test]
fn authored_fill_parse_failures_never_collapse_to_no_fill() {
    let shape = |fill: &str| {
        format!(
            r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm>{fill}<a:prstGeom prst="rect"/></p:spPr></p:sp>"#
        )
    };
    for (fill, reason) in [
        (
            r#"<a:solidFill><a:schemeClr/></a:solidFill>"#,
            "invalid_fill",
        ),
        (
            r#"<a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill>"#,
            "gradient_fill",
        ),
        (
            r#"<a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs><a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs></a:gsLst><a:path path="circle"/></a:gradFill>"#,
            "gradient_fill",
        ),
        (
            r#"<a:pattFill prst="cross"><a:fgClr><a:srgbClr val="000000"/></a:fgClr></a:pattFill>"#,
            "pattern_fill",
        ),
        (
            r#"<a:blipFill><a:blip/><a:stretch><a:fillRect/></a:stretch></a:blipFill>"#,
            "missing_relationship_target",
        ),
        (
            r#"<a:blipFill><a:blip r:link="remoteImage"/><a:stretch><a:fillRect/></a:stretch></a:blipFill>"#,
            "external_image",
        ),
    ] {
        assert_pptx_reason(&minimal_pptx(&shape(fill)), reason);
    }

    let invalid_background = replace_package_part(
        &minimal_pptx(""),
        "ppt/slides/slide1.xml",
        "<p:spTree>",
        r#"<p:bg><p:bgPr><a:solidFill><a:schemeClr/></a:solidFill></p:bgPr></p:bg><p:spTree>"#,
    );
    assert_pptx_reason(&invalid_background, "invalid_fill");

    let invalid_cell = minimal_pptx(
        r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="table"><a:tbl><a:tblGrid><a:gridCol w="1828800"/></a:tblGrid><a:tr h="914400"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p/></a:txBody><a:tcPr><a:solidFill><a:schemeClr/></a:solidFill></a:tcPr></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
    );
    assert_pptx_reason(&invalid_cell, "invalid_fill");
}

#[test]
fn invalid_picture_fill_tile_and_stretch_are_typed() {
    let png = std::fs::read("../mock-files/png/file1.png").unwrap();
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    for placement in [
        r#"<a:tile sx="0" sy="100000"/>"#,
        r#"<a:stretch/>"#,
        r#"<a:tile/><a:stretch><a:fillRect/></a:stretch>"#,
    ] {
        let shape = format!(
            r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:blipFill><a:blip r:embed="image1"/>{placement}</a:blipFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#
        );
        let bytes = pptx_with_image_resource(&shape, rels, Some(&png));
        assert_pptx_reason(&bytes, "invalid_picture_fill");
    }
}

#[test]
fn table_wide_fill_is_visible() {
    let bytes = minimal_pptx(
        r#"<p:graphicFrame><p:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="table"><a:tbl><a:tblPr><a:solidFill><a:srgbClr val="00AA44"/></a:solidFill></a:tblPr><a:tblGrid><a:gridCol w="1828800"/></a:tblGrid><a:tr h="914400"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p/></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
    );
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|positioned| matches!(
        positioned.item,
        Item::Rect { fill: Some(color), .. } if (color.r, color.g, color.b) == (0, 170, 68)
    )));
}

#[test]
fn every_drawingml_preset_pattern_is_renderable() {
    for preset in [
        "pct5",
        "pct10",
        "pct20",
        "pct25",
        "pct30",
        "pct40",
        "pct50",
        "pct60",
        "pct70",
        "pct75",
        "pct80",
        "pct90",
        "horz",
        "vert",
        "ltHorz",
        "ltVert",
        "dkHorz",
        "dkVert",
        "narHorz",
        "narVert",
        "dashHorz",
        "dashVert",
        "cross",
        "dnDiag",
        "upDiag",
        "ltDnDiag",
        "ltUpDiag",
        "dkDnDiag",
        "dkUpDiag",
        "wdDnDiag",
        "wdUpDiag",
        "dashDnDiag",
        "dashUpDiag",
        "diagCross",
        "smCheck",
        "lgCheck",
        "smGrid",
        "lgGrid",
        "dotGrid",
        "smConfetti",
        "lgConfetti",
        "horzBrick",
        "diagBrick",
        "solidDmnd",
        "openDmnd",
        "dotDmnd",
        "plaid",
        "sphere",
        "weave",
        "divot",
        "shingle",
        "wave",
        "trellis",
        "zigZag",
    ] {
        let bytes = minimal_pptx(&format!(
            r#"<p:sp><p:spPr><a:xfrm><a:off x="914400" y="914400"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:pattFill prst="{preset}"><a:fgClr><a:srgbClr val="101010"/></a:fgClr><a:bgClr><a:srgbClr val="F0F0F0"/></a:bgClr></a:pattFill><a:prstGeom prst="rect"/></p:spPr></p:sp>"#
        ));
        let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
        assert!(prof.supported, "preset {preset}: {:?}", prof.reasons);
        let doc = PptxDoc::open(&bytes, &opts()).unwrap();
        render_pptx(&doc).unwrap();
    }
}

#[test]
fn clr_map_override_is_resolved_per_slide_in_inheritance_order() {
    let bytes = color_map_override_pptx();
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(page_has_rgb(&laid.pages[0], (0, 0, 204)));
    assert!(page_has_rgb(&laid.pages[1], (0, 204, 0)));
    assert!(!page_has_rgb(&laid.pages[0], (0, 204, 0)));
    assert!(!page_has_rgb(&laid.pages[1], (0, 0, 204)));
}

#[test]
fn each_slide_uses_its_own_master_theme_placeholders_and_text_defaults() {
    let bytes = multi_master_pptx();
    let prof = profile(&bytes, OfficeFormat::Pptx).unwrap();
    assert!(prof.supported, "multi-master profile: {:?}", prof.reasons);
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(page_has_rgb(&laid.pages[0], (204, 0, 0)));
    assert!(page_has_rgb(&laid.pages[1], (0, 0, 204)));
    assert!(page_has_rgb(&laid.pages[0], (0, 170, 0)));
    assert!(page_has_rgb(&laid.pages[1], (204, 204, 0)));
    assert!(page_has_rgb(&laid.pages[0], (0, 204, 204)));
    assert!(page_has_rgb(&laid.pages[1], (204, 0, 204)));

    let slide1_runs = page_glyph_runs(&laid.pages[0]);
    let slide2_runs = page_glyph_runs(&laid.pages[1]);
    let (slide1_x, slide1_run) = slide1_runs.first().unwrap();
    let (slide2_x, slide2_run) = slide2_runs.first().unwrap();
    assert_ne!(slide1_run.face, slide2_run.face);
    assert!((slide1_run.size_pt - 30.0).abs() < 0.01);
    assert!((slide2_run.size_pt - 50.0).abs() < 0.01);
    assert!(slide2_x - slide1_x > 60.0);
    assert!(page_text(&laid.pages[0]).contains('•'));
    assert!(page_text(&laid.pages[1]).contains('→'));
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}

#[test]
fn table_style_theme_colors_are_resolved_per_slide() {
    let bytes = multi_master_pptx();
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(page_has_rgb(&laid.pages[0], (17, 17, 17)));
    assert!(page_has_rgb(&laid.pages[1], (238, 238, 238)));
}

#[test]
fn pptx_internal_slide_hyperlink_becomes_goto_page() {
    let bytes = internal_link_pptx();
    let doc = PptxDoc::open(&bytes, &opts()).unwrap();
    let (laid, _) = render_pptx(&doc).unwrap();
    assert!(laid.pages[0].items.iter().any(|item| matches!(
        &item.item,
        Item::Link {
            target: LinkTarget::Page(1),
            ..
        }
    )));
    assert!(convert(&bytes, OfficeFormat::Pptx, &opts()).is_ok());
}
