//! WML parsing integration tests (agent task T1.4).

use office_core::opc::Package;
use office_core::oxml::numbering::{NumFmt, Numbering};
use office_core::oxml::styles::StyleResolver;
use office_core::oxml::wml::{
    BlockContent, InlineContent, ParaProps, RunContent, RunProps, VMerge, parse_document,
};
use office_core::testkit::{minimal_docx, wrap_document, wrap_numbering, wrap_styles};
use std::path::Path;

/// Concatenates all text across a run's content.
fn run_text(run: &office_core::oxml::wml::Run) -> String {
    run.content
        .iter()
        .filter_map(|c| match c {
            RunContent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// Concatenates all visible text in a paragraph's runs (top-level runs only).
fn para_text(p: &office_core::oxml::wml::Paragraph) -> String {
    let mut s = String::new();
    for inline in &p.runs {
        match inline {
            InlineContent::Run(r) => s.push_str(&run_text(r)),
            InlineContent::Hyperlink { runs, .. } => {
                for r in runs {
                    s.push_str(&run_text(r));
                }
            }
            InlineContent::SimpleField(f) | InlineContent::ComplexFieldRegion(f) => {
                for r in &f.cached_runs {
                    s.push_str(&run_text(r));
                }
            }
        }
    }
    s
}

/// Every real mock `.docx` main part parses without error.
#[test]
fn parses_every_mock_docx() {
    let dir = Path::new("../mock-files/docx");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("docx") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let pkg = Package::open(&bytes).unwrap();
        let main = pkg.main_part().unwrap();
        let xml = std::str::from_utf8(pkg.part(&main).unwrap()).unwrap();
        parse_document(xml).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        checked += 1;
    }
    assert!(checked > 0);
}

/// Fixture: the style cascade resolves docDefaults → default style → basedOn
/// chain → direct properties.
#[test]
fn styles_cascade() {
    let styles = wrap_styles(
        r#"
        <w:docDefaults><w:rPrDefault><w:rPr/></w:rPrDefault></w:docDefaults>
        <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
          <w:name w:val="Normal"/><w:rPr><w:sz w:val="22"/></w:rPr>
        </w:style>
        <w:style w:type="paragraph" w:styleId="Heading1">
          <w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
          <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
          <w:rPr><w:b/><w:sz w:val="28"/></w:rPr>
        </w:style>
        "#,
    );
    let resolver = StyleResolver::parse(&styles).unwrap();

    // Heading1 paragraph, no direct run props → bold, 14pt (sz 28 half-points).
    let heading = ParaProps {
        style_id: Some("Heading1".into()),
        ..Default::default()
    };
    let effective = resolver.run_props(&heading, &RunProps::default());
    assert_eq!(effective.bold, Some(true));
    assert_eq!(effective.size_pt, Some(14.0));

    // A plain paragraph inherits the default style: 11pt, not bold.
    let plain = resolver.run_props(&ParaProps::default(), &RunProps::default());
    assert_eq!(plain.size_pt, Some(11.0));
    assert_eq!(plain.bold, None);

    // Direct run size overrides the style chain.
    let direct = RunProps {
        size_pt: Some(16.0),
        ..Default::default()
    };
    let overridden = resolver.run_props(&heading, &direct);
    assert_eq!(overridden.size_pt, Some(16.0));
    assert_eq!(overridden.bold, Some(true));

    // The resolved paragraph props carry the heading's outline level.
    let ppr = resolver.para_props(&heading);
    assert_eq!(ppr.outline_lvl, Some(0));
}

/// Fixture: numbering definitions resolve per (numId, ilvl) with formats.
#[test]
fn numbering_resolves() {
    let xml = wrap_numbering(
        r#"
        <w:abstractNum w:abstractNumId="0">
          <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
          <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/><w:suff w:val="space"/></w:lvl>
        </w:abstractNum>
        <w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num>
        "#,
    );
    let numbering = Numbering::parse(&xml).unwrap();

    let lvl0 = numbering.resolve(3, 0).expect("lvl0");
    assert_eq!(lvl0.num_fmt, NumFmt::Decimal);
    assert_eq!(lvl0.lvl_text, "%1.");
    assert_eq!(lvl0.start, 1);

    let lvl1 = numbering.resolve(3, 1).expect("lvl1");
    assert_eq!(lvl1.num_fmt, NumFmt::LowerLetter);
    assert_eq!(lvl1.lvl_text, "%2)");

    assert!(numbering.resolve(99, 0).is_none(), "dangling numId");
}

/// Fixture: a table with gridSpan and vMerge parses with the right structure.
#[test]
fn table_gridspan_vmerge() {
    let xml = wrap_document(
        r#"
        <w:tbl>
          <w:tblPr><w:tblW w:w="9000" w:type="dxa"/></w:tblPr>
          <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
          <w:tr>
            <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
            <w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
          </w:tr>
          <w:tr>
            <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc>
            <w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc>
          </w:tr>
        </w:tbl>
        "#,
    );
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Table(table) = &doc.body[0] else {
        panic!("expected a table");
    };
    assert_eq!(table.grid.len(), 3);
    assert_eq!(table.grid[0], 150.0); // 3000 twips → 150pt
    assert_eq!(table.rows.len(), 2);

    let r0c0 = &table.rows[0].cells[0];
    assert_eq!(r0c0.props.grid_span, 2);
    assert_eq!(r0c0.props.v_merge, Some(VMerge::Restart));

    let r1c0 = &table.rows[1].cells[0];
    assert_eq!(r1c0.props.v_merge, Some(VMerge::Continue));
}

/// Fixture: a complex field (fldChar begin/separate/end) assembles into one
/// field with instruction text and cached result runs.
#[test]
fn complex_field_assembly() {
    let xml = wrap_document(
        r#"
        <w:p>
          <w:r><w:fldChar w:fldCharType="begin"/></w:r>
          <w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>
          <w:r><w:fldChar w:fldCharType="separate"/></w:r>
          <w:r><w:t>7</w:t></w:r>
          <w:r><w:fldChar w:fldCharType="end"/></w:r>
        </w:p>
        "#,
    );
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Paragraph(p) = &doc.body[0] else {
        panic!("expected a paragraph");
    };
    let field = p
        .runs
        .iter()
        .find_map(|c| match c {
            InlineContent::ComplexFieldRegion(f) => Some(f),
            _ => None,
        })
        .expect("complex field region");
    assert_eq!(field.instruction.trim(), "PAGE");
    assert_eq!(field.cached_runs.len(), 1);
    assert_eq!(run_text(&field.cached_runs[0]), "7");
}

/// Fixture: a final section with header references and custom page geometry.
#[test]
fn sectpr_with_headers() {
    let xml = wrap_document(
        r#"
        <w:p><w:r><w:t>body</w:t></w:r></w:p>
        <w:sectPr>
          <w:headerReference w:type="default" r:id="rId4"/>
          <w:headerReference w:type="first" r:id="rId5"/>
          <w:footerReference w:type="default" r:id="rId6"/>
          <w:pgSz w:w="16840" w:h="11900" w:orient="landscape"/>
          <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/>
          <w:titlePg/>
        </w:sectPr>
        "#,
    );
    let doc = parse_document(&xml).unwrap();
    let sect = &doc.final_sect;
    assert_eq!(sect.header_refs.len(), 2);
    assert_eq!(sect.header_refs[0].rel_id, "rId4");
    assert_eq!(sect.footer_refs.len(), 1);
    assert!(sect.title_pg);
    assert_eq!(
        sect.orientation,
        office_core::oxml::wml::Orientation::Landscape
    );
    assert!((sect.page_w_pt - 842.0).abs() < 0.01); // 16840 twips → 842pt
    assert_eq!(sect.margins.header, 36.0); // 720 twips → 36pt
}

/// Fixture: tracked changes render as final view — inserted text kept, deleted
/// text dropped.
#[test]
fn tracked_changes_final_view() {
    let xml = wrap_document(
        r#"
        <w:p>
          <w:r><w:t>base </w:t></w:r>
          <w:ins w:id="1" w:author="x"><w:r><w:t>added</w:t></w:r></w:ins>
          <w:del w:id="2" w:author="x"><w:r><w:delText>removed</w:delText></w:r></w:del>
          <w:r><w:t> tail</w:t></w:r>
        </w:p>
        "#,
    );
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Paragraph(p) = &doc.body[0] else {
        panic!("expected a paragraph");
    };
    let text = para_text(p);
    assert!(text.contains("added"), "inserted text kept: {text:?}");
    assert!(!text.contains("removed"), "deleted text dropped: {text:?}");
    assert_eq!(text, "base added tail");
}

/// A minimal docx round-trips through the package and parses.
#[test]
fn minimal_docx_parses() {
    let bytes = minimal_docx(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
    let pkg = Package::open(&bytes).unwrap();
    let xml = std::str::from_utf8(pkg.part("word/document.xml").unwrap()).unwrap();
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.body.len(), 1);
}
