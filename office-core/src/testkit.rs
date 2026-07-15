//! Fixture builders shared by unit and integration tests. Compiled into the
//! library so `tests/*.rs` can use them; not part of the stable public API.
//!
//! Fixtures are assembled as real OPC (ZIP) packages so they exercise the same
//! [`crate::opc`] path production files take.

use std::io::{Cursor, Write};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// The namespace declarations placed on the root of every generated
/// `document.xml` so fixtures can use any common OOXML prefix.
pub const DOC_NAMESPACES: &str = concat!(
    r#" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#,
    r#" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#,
    r#" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#,
    r#" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#,
    r#" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#,
    r#" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#,
    r#" xmlns:v="urn:schemas-microsoft-com:vml""#,
);

/// Zips the given `(part_name, bytes)` entries into an OPC package (Deflate).
pub fn zip_package(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zw = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in parts {
            zw.start_file(*name, opts).expect("start_file");
            zw.write_all(data).expect("write part");
        }
        zw.finish().expect("finish zip");
    }
    buf
}

/// Wraps body-level XML in a complete `word/document.xml` (with a final section).
pub fn wrap_document(body_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document{ns}><w:body>{body}</w:body></w:document>"#,
        ns = DOC_NAMESPACES,
        body = body_inner,
    )
}

/// Wraps run/paragraph XML in a complete `word/styles.xml` document.
pub fn wrap_styles(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles{ns}>{inner}</w:styles>"#,
        ns = DOC_NAMESPACES,
    )
}

/// Wraps level XML in a complete `word/numbering.xml` document.
pub fn wrap_numbering(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering{ns}>{inner}</w:numbering>"#,
        ns = DOC_NAMESPACES,
    )
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// Builds a complete, openable minimal `.docx` package around body XML.
pub fn minimal_docx(body_inner: &str) -> Vec<u8> {
    let document = wrap_document(body_inner);
    zip_package(&[
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

const IMAGE_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const IMAGE_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdImg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;

fn image_docx(body_inner: &str) -> Vec<u8> {
    let document = wrap_document(body_inner);
    zip_package(&[
        ("[Content_Types].xml", IMAGE_CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", IMAGE_RELS.as_bytes()),
        (
            "word/media/image1.png",
            include_bytes!("../../mock-files/png/file1.png"),
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
fn picture_anchor(
    horizontal_origin: &str,
    horizontal_position: &str,
    vertical_origin: &str,
    vertical_position: &str,
    width_emu: i64,
    height_emu: i64,
    wrap: &str,
    relative_height: u32,
) -> String {
    format!(
        r#"<w:drawing><wp:anchor simplePos="0" relativeHeight="{relative_height}" behindDoc="0" allowOverlap="1" distT="0" distB="114300" distL="114300" distR="114300"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="{horizontal_origin}">{horizontal_position}</wp:positionH><wp:positionV relativeFrom="{vertical_origin}">{vertical_position}</wp:positionV><wp:extent cx="{width_emu}" cy="{height_emu}"/>{wrap}<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><a:blip r:embed="rIdImg"/></a:graphicData></a:graphic></wp:anchor></w:drawing>"#
    )
}

/// Margin/top-left page float with enough body text to exercise wrap release.
pub fn margin_top_left_float_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let lead = (0..9)
        .map(|index| {
            format!(
                "<w:p><w:r><w:t>Wrapped body line {index} with enough words to expose its text window.</w:t></w:r></w:p>"
            )
        })
        .collect::<String>();
    image_docx(&format!(
        "{lead}<w:p><w:r>{anchor}</w:r></w:p><w:p><w:r><w:t>Below float band</w:t></w:r></w:p>"
    ))
}

/// Simultaneous left and right page floats carried by one paragraph.
pub fn opposite_page_floats_docx() -> Vec<u8> {
    let left = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let right = picture_anchor(
        "margin",
        "<wp:align>right</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        2,
    );
    image_docx(&format!(
        "<w:p><w:r><w:t>Text in the middle window between simultaneous page floats.</w:t></w:r></w:p><w:p><w:r>{left}{right}</w:r></w:p>"
    ))
}

/// Two same-side page-float exclusions that overlap vertically.
pub fn overlapping_page_floats_docx() -> Vec<u8> {
    let left = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    image_docx(&format!("<w:p><w:r>{left}{left}</w:r></w:p>"))
}

/// Opposite-side page floats whose combined bands leave under 36pt for text.
pub fn page_floats_without_text_window_docx() -> Vec<u8> {
    let left = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        2_921_000,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let right = picture_anchor(
        "margin",
        "<wp:align>right</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        2_921_000,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        2,
    );
    image_docx(&format!("<w:p><w:r>{left}{right}</w:r></w:p>"))
}

/// Full-width top-and-bottom page band overlapping a side band.
pub fn full_and_side_page_floats_docx() -> Vec<u8> {
    let full = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        254_000,
        977_900,
        "<wp:wrapTopAndBottom/>",
        1,
    );
    let side = picture_anchor(
        "margin",
        "<wp:align>right</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        2,
    );
    image_docx(&format!("<w:p><w:r>{full}{side}</w:r></w:p>"))
}

/// A page-scoped top-and-bottom float that moves all intersecting body text below it.
pub fn page_top_bottom_float_docx() -> Vec<u8> {
    let full = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        254_000,
        914_400,
        "<wp:wrapTopAndBottom/>",
        1,
    );
    image_docx(&format!(
        "<w:p><w:r><w:t>Body below page band</w:t></w:r></w:p><w:p><w:r>{full}</w:r></w:p>"
    ))
}

/// Page-relative centered positioning on both axes.
pub fn page_center_float_docx() -> Vec<u8> {
    let centered = picture_anchor(
        "page",
        "<wp:align>center</wp:align>",
        "page",
        "<wp:align>center</wp:align>",
        254_000,
        254_000,
        "<wp:wrapNone/>",
        1,
    );
    image_docx(&format!("<w:p><w:r>{centered}</w:r></w:p>"))
}

/// Column-offset, paragraph-relative top-and-bottom float (file-1 anchor C shape).
pub fn paragraph_column_top_bottom_float_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "column",
        "<wp:posOffset>1931670</wp:posOffset>",
        "paragraph",
        "<wp:posOffset>6985</wp:posOffset>",
        254_000,
        241_300,
        "<wp:wrapTopAndBottom/>",
        1,
    );
    image_docx(&format!(
        "<w:p><w:r>{anchor}</w:r></w:p><w:p><w:r><w:t>Text below top-and-bottom band</w:t></w:r></w:p>"
    ))
}

/// Two paragraph-relative floats in one paragraph, outside the bounded envelope.
pub fn two_paragraph_floats_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "paragraph",
        "<wp:posOffset>0</wp:posOffset>",
        254_000,
        254_000,
        "<wp:wrapNone/>",
        1,
    );
    image_docx(&format!("<w:p><w:r>{anchor}{anchor}</w:r></w:p>"))
}

/// An anchored drawing governed by a two-column final section.
pub fn multicolumn_anchor_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "paragraph",
        "<wp:posOffset>0</wp:posOffset>",
        254_000,
        254_000,
        "<wp:wrapNone/>",
        1,
    );
    image_docx(&format!(
        "<w:p><w:r>{anchor}</w:r></w:p><w:sectPr><w:cols w:num=\"2\"/></w:sectPr>"
    ))
}

/// Text-relative left floating table with file-1's 187-twip right clearance.
pub fn floating_table_side_wrap_docx(table_width_twips: u32) -> Vec<u8> {
    let filler = "side wrapped words ".repeat(30);
    minimal_docx(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{table_width_twips}" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="0" w:tblpY="1" w:rightFromText="187"/></w:tblPr><w:tblGrid><w:gridCol w:w="{table_width_twips}"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Floating table</w:t></w:r></w:p><w:p><w:r><w:t>Second table row</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>{filler}</w:t></w:r></w:p><w:p><w:r><w:t>Below floating table</w:t></w:r></w:p>"#
    ))
}

/// A page float near a pagination boundary whose page assignment oscillates.
pub fn non_convergent_page_float_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        3_810_000,
        5_080_000,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let lead = (0..20)
        .map(|index| {
            format!(
                "<w:p><w:r><w:t>Boundary {index} {}</w:t></w:r></w:p>",
                "boundary words ".repeat(10)
            )
        })
        .collect::<String>();
    image_docx(&format!("{lead}<w:p><w:r>{anchor}</w:r></w:p>"))
}

/// A page float where reflow grows the first paragraph past the band bottom.
/// The sentinel paragraph must use its new Y, not the previous pass's stale Y.
pub fn stale_page_band_schedule_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        2_921_000,
        698_500,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let growing = "Growing paragraph words for the page band fixpoint. ".repeat(5);
    image_docx(&format!(
        "<w:p><w:r><w:t>{growing}</w:t></w:r></w:p><w:p><w:r><w:t>Schedule sentinel</w:t></w:r></w:p><w:p><w:r>{anchor}</w:r></w:p>"
    ))
}

/// Two vertically disjoint left page-float bands crossed by one long paragraph.
pub fn disjoint_left_page_floats_docx() -> Vec<u8> {
    let upper = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:posOffset>0</wp:posOffset>",
        977_900,
        635_000,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let lower = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:posOffset>2540000</wp:posOffset>",
        977_900,
        635_000,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        2,
    );
    let body =
        "Disjoint band paragraph words keep flowing across both separated exclusion regions. "
            .repeat(25);
    image_docx(&format!(
        "<w:p><w:r><w:t>{body}</w:t></w:r></w:p><w:p><w:r>{upper}{lower}</w:r></w:p>"
    ))
}

/// A continuous two-column boundary carried by the anchored paragraph itself.
pub fn continuous_multicolumn_anchor_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "paragraph",
        "<wp:posOffset>0</wp:posOffset>",
        254_000,
        254_000,
        "<wp:wrapNone/>",
        1,
    );
    image_docx(&format!(
        r#"<w:p><w:r><w:t>Single-column lead</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:type w:val="continuous"/><w:cols w:num="2"/></w:sectPr></w:pPr><w:r>{anchor}</w:r></w:p><w:sectPr><w:cols w:num="1"/></w:sectPr>"#
    ))
}

/// Paragraph-relative vertical anchor whose horizontal origin is the page box.
pub fn paragraph_float_with_page_horizontal_origin_docx(aligned: bool) -> Vec<u8> {
    let horizontal = if aligned {
        "<wp:align>right</wp:align>"
    } else {
        "<wp:posOffset>228600</wp:posOffset>"
    };
    let anchor = picture_anchor(
        "page",
        horizontal,
        "paragraph",
        "<wp:posOffset>0</wp:posOffset>",
        254_000,
        254_000,
        "<wp:wrapNone/>",
        1,
    );
    image_docx(&format!("<w:p><w:r>{anchor}</w:r></w:p>"))
}

/// Page float in a vertically centered or bottom-aligned body section.
pub fn vertically_aligned_body_page_float_docx(alignment: &str) -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    image_docx(&format!(
        r#"<w:p><w:r><w:t>Vertically aligned body text</w:t></w:r></w:p><w:p><w:r>{anchor}</w:r></w:p><w:sectPr><w:vAlign w:val="{alignment}"/></w:sectPr>"#
    ))
}

/// A page float carried by a continuous, vertically aligned section boundary.
/// The final top-aligned section would otherwise swallow this unsupported span.
pub fn continuous_vertically_aligned_page_float_docx(alignment: &str) -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        977_900,
        977_900,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    image_docx(&format!(
        r#"<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/><w:vAlign w:val="{alignment}"/></w:sectPr></w:pPr><w:r><w:t>Continuous vertically aligned text</w:t>{anchor}</w:r></w:p><w:sectPr><w:vAlign w:val="top"/></w:sectPr>"#
    ))
}

/// A long paragraph begins near the bottom of page one under a lower side
/// band, then continues onto page two where that page-one band must not apply.
pub fn cross_page_page_float_band_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:posOffset>7315200</wp:posOffset>",
        977_900,
        914_400,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    let lead = (0..43)
        .map(|index| format!("<w:p><w:r><w:t>Cross-page lead {index}</w:t></w:r></w:p>"))
        .collect::<String>();
    let body = "Continuation words must use only the bands on their physical page. ".repeat(90);
    image_docx(&format!(
        "<w:p><w:r>{anchor}</w:r></w:p>{lead}<w:p><w:r><w:t>{body}</w:t></w:r></w:p>"
    ))
}

/// A full-width band ends exactly where a left-side band begins. Lines moved
/// below the first exclusion must be rebroken against the second exclusion.
pub fn stacked_full_then_side_page_floats_docx() -> Vec<u8> {
    let full = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        254_000,
        762_000,
        r#"<wp:wrapTopAndBottom/>"#,
        1,
    );
    let side = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:posOffset>876300</wp:posOffset>",
        977_900,
        1_016_000,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        2,
    );
    let body = "Shifted side-band text must be narrowed after full-band clearance. ".repeat(8);
    image_docx(&format!(
        "<w:p><w:r><w:t>{body}</w:t></w:r></w:p><w:p><w:r>{full}{side}</w:r></w:p>"
    ))
}

/// Floating table with independently authored horizontal origin and offset.
pub fn floating_table_origin_docx(horizontal_anchor: &str, x_twips: i32) -> Vec<u8> {
    let filler = "Resolved-origin following words ".repeat(20);
    minimal_docx(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="2880" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="{horizontal_anchor}" w:tblpX="{x_twips}" w:tblpY="0" w:rightFromText="187"/></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Resolved origin table</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>{filler}</w:t></w:r></w:p>"#
    ))
}

/// Floating table with independently authored left/right/bottom text distances.
pub fn floating_table_distances_docx(
    side: &str,
    left_twips: u32,
    right_twips: u32,
    bottom_twips: u32,
) -> Vec<u8> {
    let x_twips = if side == "right" { 6480 } else { 0 };
    let filler = "distance flow words ".repeat(90);
    minimal_docx(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="2880" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="{x_twips}" w:tblpY="0" w:leftFromText="{left_twips}" w:rightFromText="{right_twips}" w:bottomFromText="{bottom_twips}"/></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Distance table</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>{filler}</w:t></w:r></w:p>"#
    ))
}

/// Left-hugging floating table followed by a first-line-indented paragraph.
pub fn floating_table_first_line_indent_docx() -> Vec<u8> {
    let filler = "first-line indent beside a floating table ".repeat(30);
    minimal_docx(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="2880" w:type="dxa"/><w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="0" w:tblpY="0" w:rightFromText="180"/></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="1440" w:hRule="exact"/></w:trPr><w:tc><w:p><w:r><w:t>Indent table</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:pPr><w:ind w:firstLine="432"/></w:pPr><w:r><w:t>{filler}</w:t></w:r></w:p>"#
    ))
}

/// One left page band with less than 36pt remaining; no opposite band exists.
pub fn single_narrow_page_float_docx() -> Vec<u8> {
    let anchor = picture_anchor(
        "margin",
        "<wp:align>left</wp:align>",
        "margin",
        "<wp:align>top</wp:align>",
        5_715_000,
        635_000,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        1,
    );
    image_docx(&format!(
        "<w:p><w:r><w:t>Narrow surviving text window</w:t></w:r></w:p><w:p><w:r>{anchor}</w:r></w:p>"
    ))
}

/// Floating table used to freeze legacy block-flow height semantics.
pub fn floating_table_block_flow_docx(
    table_width_twips: u32,
    vertical_anchor: &str,
    side_clearance_twips: u32,
) -> Vec<u8> {
    minimal_docx(&format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{table_width_twips}" w:type="dxa"/><w:tblpPr w:vertAnchor="{vertical_anchor}" w:horzAnchor="text" w:tblpX="0" w:tblpY="1" w:leftFromText="{side_clearance_twips}" w:rightFromText="0"/></w:tblPr><w:tblGrid><w:gridCol w:w="{table_width_twips}"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Legacy block table</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>Legacy next paragraph</w:t></w:r></w:p>"#
    ))
}

/// Builds a compact Google-Docs-style DOCX conformance fixture.
///
/// The package intentionally combines producer idioms that are individually
/// invisible or supported in final view, so parser regressions cannot silently
/// shrink the profile envelope again.
pub fn gdocs_paragraph_docx() -> Vec<u8> {
    let document = wrap_document(
        r#"
        <w:p><w:pPr><w:rPr/></w:pPr><w:r><w:t>GDocs paragraph probe</w:t></w:r></w:p>
        <w:p>
          <w:pPr>
            <w:jc w:val="left"/>
            <w:ind w:firstLine="192.00000000000003"/>
            <w:rPr><w:color w:val="336699"/><w:sz w:val="22.0"/><w:vertAlign w:val="baseline"/></w:rPr>
          </w:pPr>
          <w:r>
            <w:rPr>
              <w:b w:val="0"/><w:i w:val="0"/><w:smallCaps w:val="0"/>
              <w:strike w:val="0"/><w:u w:val="none"/><w:rtl w:val="0"/>
            </w:rPr>
            <w:t>GDocs toggles probe</w:t>
          </w:r>
        </w:p>
        <w:tbl>
          <w:tblPr><w:tblInd w:w="-108.0" w:type="dxa"/></w:tblPr>
          <w:tblGrid>
            <w:gridCol w:w="2400.5"/>
            <w:tblGridChange w:id="1"><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid></w:tblGridChange>
          </w:tblGrid>
          <w:tr>
            <w:trPr><w:tblHeader w:val="0"/><w:cantSplit w:val="0"/></w:trPr>
            <w:tc><w:tcPr/><w:p><w:r><w:t>GDocs table probe</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>
        <w:sectPr>
          <w:footerReference w:type="default" r:id="rIdFooter"/>
          <w:pgNumType w:start="1" w:fmt="decimal"/>
        </w:sectPr>
        "#,
    );
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>
</Types>"#;
    let document_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdFooter" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
</Relationships>"#;
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr{ns}><w:p><w:r><w:t>GDocs footer probe</w:t></w:r></w:p></w:ftr>"#,
        ns = DOC_NAMESPACES,
    );
    zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels),
        ("word/footer1.xml", footer.as_bytes()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opc::Package;

    #[test]
    fn minimal_docx_opens_and_finds_main_part() {
        let bytes = minimal_docx(r#"<w:p><w:r><w:t>Hi</w:t></w:r></w:p>"#);
        let pkg = Package::open(&bytes).expect("open");
        assert_eq!(pkg.main_part().unwrap(), "word/document.xml");
        assert!(pkg.part("word/document.xml").is_some());
    }
}
