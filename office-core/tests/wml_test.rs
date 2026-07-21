//! WML parsing integration tests (agent task T1.4).

use office_core::opc::Package;
use office_core::oxml::numbering::{NumFmt, Numbering};
use office_core::oxml::styles::StyleResolver;
use office_core::oxml::wml::{
    BlockContent, InlineContent, NoteKind, ParaProps, RunContent, RunProps, VMerge, parse_document,
    parse_hdr_ftr, parse_notes,
};
use office_core::testkit::{minimal_docx, wrap_document, wrap_numbering, wrap_styles};
use office_core::{OfficeError, UnsupportedCode};
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
    fn inline_text(inlines: &[InlineContent], output: &mut String) {
        for inline in inlines {
            match inline {
                InlineContent::Bookmark(_) => {}
                InlineContent::Run(run) => output.push_str(&run_text(run)),
                InlineContent::Hyperlink { runs, .. } => {
                    for run in runs {
                        output.push_str(&run_text(run));
                    }
                }
                InlineContent::SimpleField(field) | InlineContent::ComplexFieldRegion(field) => {
                    inline_text(&field.content, output);
                }
                InlineContent::Math(math) => output.push_str(&math.source_text()),
            }
        }
    }
    let mut output = String::new();
    inline_text(&p.runs, &mut output);
    output
}

#[test]
fn paragraph_numbering_rejects_ilvl_above_eight() {
    let xml = wrap_document(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="9"/><w:numId w:val="1"/></w:numPr></w:pPr></w:p>"#,
    );

    let error = parse_document(&xml).expect_err("reject out-of-range paragraph ilvl");

    assert!(matches!(
        error,
        OfficeError::Unsupported {
            code: UnsupportedCode::ResourceLimit,
            ..
        }
    ));
}

/// Every real mock `.docx` main part parses without error.
#[test]
fn parses_every_mock_docx() {
    let dir = Path::new("../mock-files/docx");
    let entries = std::fs::read_dir(dir).expect("required mock-files/docx fixtures");
    let mut checked = 0;
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("read fixture under {}: {error}", dir.display()))
            .path();
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
    assert!(checked > 0, "no required docx fixtures found");
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

#[test]
fn first_line_and_hanging_indents_clear_each_other_across_cascade_levels() {
    let inherited_first = ParaProps {
        indent: office_core::oxml::wml::Indent {
            first_line_pt: Some(21.6),
            ..Default::default()
        },
        ..Default::default()
    };
    let direct_hanging = ParaProps {
        indent: office_core::oxml::wml::Indent {
            hanging_pt: Some(36.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let hanging = direct_hanging.merge_over(&inherited_first);
    assert_eq!(hanging.indent.first_line_pt, None);
    assert_eq!(hanging.indent.hanging_pt, Some(36.0));

    let inherited_hanging = direct_hanging;
    let direct_first = inherited_first;
    let first = direct_first.merge_over(&inherited_hanging);
    assert_eq!(first.indent.first_line_pt, Some(21.6));
    assert_eq!(first.indent.hanging_pt, None);
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

fn assert_resource_limit(error: OfficeError) {
    assert!(
        matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::ResourceLimit,
                ..
            }
        ),
        "expected resource limit, got {error}"
    );
}

#[test]
fn rejects_excessive_word_structure_before_layout() {
    let cases = [
        r#"<w:p/><w:sectPr><w:cols w:num="4294967295"/></w:sectPr>"#.to_string(),
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="100"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="4294967295"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#.to_string(),
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="100"/></w:tblGrid><w:tr><w:trPr><w:gridBefore w:val="200"/><w:gridAfter w:val="200"/></w:trPr><w:tc><w:tcPr><w:gridSpan w:val="200"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#.to_string(),
        format!(
            "<w:tbl><w:tblGrid>{}</w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>",
            r#"<w:gridCol w:w="100"/>"#.repeat(257)
        ),
    ];

    for body in cases {
        let error = parse_document(&wrap_document(&body)).expect_err("structure limit");
        assert_resource_limit(error);
    }
}

#[test]
fn rejects_authored_tab_limit_plus_one_in_every_word_story() {
    let tabs = r#"<w:tab w:val="left" w:pos="720"/>"#.repeat(65);
    let paragraph = format!("<w:p><w:pPr><w:tabs>{tabs}</w:tabs></w:pPr></w:p>");

    assert_resource_limit(
        parse_document(&wrap_document(&paragraph)).expect_err("document tab limit"),
    );

    let header = format!(
        r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{paragraph}</w:hdr>"#
    );
    assert_resource_limit(parse_hdr_ftr(&header).expect_err("header tab limit"));

    let notes = format!(
        r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1">{paragraph}</w:footnote></w:footnotes>"#
    );
    assert_resource_limit(parse_notes(&notes, NoteKind::Footnote).expect_err("note tab limit"));
}

#[test]
fn decimal_twip_measurements_preserve_table_grid_and_paragraph_indent() {
    let xml = wrap_document(
        r#"
        <w:tbl>
          <w:tblGrid><w:gridCol w:w="1200.5"/></w:tblGrid>
          <w:tr><w:tc><w:p/></w:tc></w:tr>
        </w:tbl>
        <w:p><w:pPr><w:ind w:firstLine="192.00000000000003" w:hanging="-108.0"/></w:pPr></w:p>
        "#,
    );
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Table(table) = &doc.body[0] else {
        panic!("expected table");
    };
    assert_eq!(table.grid.len(), 1);
    assert!(
        (table.grid[0] - 60.025).abs() < 1e-12,
        "grid={:?}",
        table.grid
    );
    let BlockContent::Paragraph(paragraph) = &doc.body[1] else {
        panic!("expected paragraph");
    };
    assert!((paragraph.props.indent.first_line_pt.unwrap() - 9.6).abs() < 1e-12);
    assert_eq!(paragraph.props.indent.hanging_pt, Some(-5.4));
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
    assert_eq!(field.content.len(), 1);
    let InlineContent::Run(run) = &field.content[0] else {
        panic!("expected cached field run");
    };
    assert_eq!(run_text(run), "7");
    assert!(!field.starved);
}

#[test]
fn complex_field_result_hyperlink_is_segmented_across_paragraphs() {
    let xml = wrap_document(
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
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Paragraph(entry) = &doc.body[1] else {
        panic!("expected the TOC entry paragraph");
    };
    assert!(
        entry
            .runs
            .iter()
            .any(|inline| matches!(inline, InlineContent::ComplexFieldRegion(_))),
        "the hyperlink result must remain inside a complex-field segment: {:?}",
        entry.runs
    );
    assert_eq!(para_text(entry), "Heading 1\t3");
}

#[test]
fn paragraph_mark_empty_run_properties_are_parsed_as_supported_bookkeeping() {
    let xml = wrap_document(r#"<w:p><w:pPr><w:rPr/></w:pPr></w:p>"#);
    let doc = parse_document(&xml).unwrap();
    let BlockContent::Paragraph(paragraph) = &doc.body[0] else {
        panic!("expected a paragraph");
    };
    assert!(
        paragraph.props.visible_unsupported.is_empty(),
        "empty paragraph-mark properties must not poison the paragraph: {:?}",
        paragraph.props.visible_unsupported
    );
}

#[test]
fn explicit_toggle_matrix_obeys_on_off_semantics_across_wml_scopes() {
    for value in [
        None,
        Some("1"),
        Some("true"),
        Some("0"),
        Some("false"),
        Some("off"),
    ] {
        let attr = value.map_or_else(String::new, |value| format!(r#" w:val="{value}""#));
        let expected = !matches!(value, Some("0" | "false" | "off"));
        let toggle = |name: &str| format!("<w:{name}{attr}/>");
        let run_props = [
            "b",
            "i",
            "caps",
            "smallCaps",
            "strike",
            "dstrike",
            "vanish",
            "rtl",
        ]
        .into_iter()
        .map(toggle)
        .collect::<String>();
        let para_props = [
            "keepNext",
            "keepLines",
            "pageBreakBefore",
            "widowControl",
            "bidi",
            "contextualSpacing",
        ]
        .into_iter()
        .map(toggle)
        .collect::<String>();
        let row_props = ["tblHeader", "cantSplit", "hidden"]
            .into_iter()
            .map(toggle)
            .collect::<String>();
        let xml = wrap_document(&format!(
            r#"<w:p><w:pPr>{para_props}</w:pPr><w:r><w:rPr>{run_props}</w:rPr><w:t>x</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="1000"/></w:tblGrid><w:tr><w:trPr>{row_props}</w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:sectPr>{}</w:sectPr>"#,
            toggle("titlePg")
        ));
        let doc = parse_document(&xml).unwrap();
        let BlockContent::Paragraph(paragraph) = &doc.body[0] else {
            panic!("expected paragraph");
        };
        let InlineContent::Run(run) = &paragraph.runs[0] else {
            panic!("expected run");
        };
        let BlockContent::Table(table) = &doc.body[1] else {
            panic!("expected table");
        };
        let actual = vec![
            run.props.bold,
            run.props.italic,
            run.props.caps,
            run.props.small_caps,
            run.props.strike,
            run.props.dstrike,
            run.props.vanish,
            run.props.rtl,
            Some(paragraph.props.keep_next),
            Some(paragraph.props.keep_lines),
            Some(paragraph.props.page_break_before),
            paragraph.props.widow_control,
            paragraph.props.bidi,
            Some(paragraph.props.contextual_spacing),
            Some(table.rows[0].props.header),
            Some(table.rows[0].props.cant_split),
            Some(table.rows[0].props.hidden),
            Some(doc.final_sect.title_pg),
        ];
        assert_eq!(actual, vec![Some(expected); 18], "w:val={value:?}");
    }
}

#[test]
fn inline_and_block_level_bookmarks_are_captured_safely() {
    let xml = wrap_document(
        r#"<w:bookmarkStart w:id="1" w:name="before-paragraph"/><w:p><w:bookmarkStart w:id="2" w:name="inside-paragraph"/><w:bookmarkStart w:id="3"/><w:r><w:t>Target</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p><w:bookmarkEnd w:id="1"/>"#,
    );
    let doc = parse_document(&xml).expect("bookmark parse");
    let BlockContent::Paragraph(paragraph) = &doc.body[0] else {
        panic!("expected paragraph");
    };
    let bookmarks: Vec<_> = paragraph
        .runs
        .iter()
        .filter_map(|inline| match inline {
            InlineContent::Bookmark(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(bookmarks, ["before-paragraph", "inside-paragraph"]);
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

/// Cascade precedence: a character style overrides the paragraph style for the
/// same property (docDefaults → default style → para style → CHAR style → direct).
#[test]
fn character_style_overrides_paragraph_style() {
    let styles = wrap_styles(
        r#"
        <w:style w:type="paragraph" w:styleId="P">
          <w:name w:val="P"/><w:rPr><w:sz w:val="20"/><w:b/></w:rPr>
        </w:style>
        <w:style w:type="character" w:styleId="C">
          <w:name w:val="C"/><w:rPr><w:sz w:val="40"/></w:rPr>
        </w:style>
        "#,
    );
    let resolver = StyleResolver::parse(&styles).unwrap();
    let para = ParaProps {
        style_id: Some("P".into()),
        ..Default::default()
    };
    let run = RunProps {
        style_id: Some("C".into()),
        ..Default::default()
    };
    let eff = resolver.run_props(&para, &run);
    // Character style C (sz 40 → 20pt) overrides paragraph style P (sz 20 → 10pt).
    assert_eq!(
        eff.size_pt,
        Some(20.0),
        "character style must win over paragraph style"
    );
    // A property only P sets (bold) still inherits through.
    assert_eq!(
        eff.bold,
        Some(true),
        "paragraph-style bold still applies when C is silent"
    );
    // Direct run props override even the character style.
    let direct = RunProps {
        style_id: Some("C".into()),
        size_pt: Some(9.0),
        ..Default::default()
    };
    assert_eq!(
        resolver.run_props(&para, &direct).size_pt,
        Some(9.0),
        "direct wins over all"
    );
}
