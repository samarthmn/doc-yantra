use office_core::{
    ConvertOptions, EmbeddingPermission, FontExactness, FontExpectation, FontResolutionEvent,
    OfficeError, OfficeFormat, ResolutionMode, UnsupportedCode, convert, font_requests,
    fonts::FontDb, fonts::resolve, fonts::resolve_exact_with_stretch, fonts::validate_font,
    profile_with_options, register_font_validated,
};
use sha2::{Digest, Sha256};

const CARLITO: &[u8] = include_bytes!("../fonts/carlito/Carlito-Regular.ttf");

#[test]
fn font_resolution_defaults_are_compatible_and_do_not_scan_system_fonts() {
    let options = ConvertOptions::default();

    assert_eq!(options.resolution_mode, ResolutionMode::Compatible);
    assert!(!options.include_system_fonts);
}

#[test]
fn validated_registration_returns_a_reproducible_font_identity() {
    let sha256 = format!("{:x}", Sha256::digest(CARLITO));
    let expectation = FontExpectation {
        family: "Carlito".to_string(),
        weight: 400,
        italic: false,
        stretch: 5,
        version: None,
        sha256: Some(sha256.clone()),
        face_index: Some(0),
        required_scripts: Vec::new(),
        required_codepoints: Vec::new(),
        vf_coords: Vec::new(),
    };

    let identity = register_font_validated(CARLITO, Some(expectation))
        .expect("validated Carlito registration");

    assert!(identity.families.iter().any(|family| family == "Carlito"));
    assert_eq!(identity.weight, 400);
    assert!(!identity.italic);
    assert_eq!(identity.stretch, 5);
    assert_eq!(identity.face_count, 1);
    assert_eq!(identity.face_index, 0);
    assert_eq!(identity.sha256, sha256);
    assert_eq!(identity.fs_type, EmbeddingPermission::Installable);
    assert!(identity.coverage_ok);
}

#[test]
fn validated_registration_rejects_identity_and_hash_mismatches() {
    let wrong_family = FontExpectation {
        family: "Ubuntu".to_string(),
        weight: 400,
        italic: false,
        stretch: 5,
        ..FontExpectation::default()
    };
    let error = register_font_validated(CARLITO, Some(wrong_family))
        .expect_err("the name table must be authoritative");
    assert!(matches!(
        error,
        OfficeError::Unsupported {
            code: UnsupportedCode::FontIdentityMismatch,
            ..
        }
    ));

    let wrong_hash = FontExpectation {
        family: "Carlito".to_string(),
        weight: 400,
        italic: false,
        stretch: 5,
        sha256: Some("00".repeat(32)),
        ..FontExpectation::default()
    };
    let error = register_font_validated(CARLITO, Some(wrong_hash))
        .expect_err("a pinned digest mismatch must fail closed");
    assert!(matches!(
        error,
        OfficeError::Unsupported {
            code: UnsupportedCode::FontHashMismatch,
            ..
        }
    ));
}

#[test]
fn validated_registration_rejects_corrupt_and_oversized_inputs() {
    assert!(register_font_validated(b"not a font", None).is_err());

    let oversized = vec![0_u8; 24 * 1024 * 1024 + 1];
    let error = register_font_validated(&oversized, None)
        .expect_err("the default registration cap is 24 MiB");
    assert!(matches!(
        error,
        OfficeError::Unsupported {
            code: UnsupportedCode::DecompressionLimit,
            ..
        }
    ));
}

#[test]
fn validated_registration_rejects_all_mismatched_constraints_and_accepts_ttc_index_zero() {
    let base = FontExpectation {
        family: "Carlito".to_string(),
        weight: 400,
        italic: false,
        stretch: 5,
        ..FontExpectation::default()
    };
    for expectation in [
        FontExpectation {
            weight: 700,
            ..base.clone()
        },
        FontExpectation {
            version: Some("Version 999".to_string()),
            ..base.clone()
        },
        FontExpectation {
            vf_coords: vec![office_core::VariationCoordinate {
                axis: "wght".to_string(),
                value: 400.0,
            }],
            ..base.clone()
        },
    ] {
        let error = register_font_validated(CARLITO, Some(expectation))
            .expect_err("identity constraint must fail closed");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::FontIdentityMismatch,
                ..
            }
        ));
    }

    let coverage = FontExpectation {
        required_scripts: vec!["cjk".to_string()],
        ..base.clone()
    };
    assert!(matches!(
        register_font_validated(CARLITO, Some(coverage)),
        Err(OfficeError::Unsupported {
            code: UnsupportedCode::FontCoverageMismatch,
            ..
        })
    ));
    let bad_index = FontExpectation {
        face_index: Some(1),
        ..base
    };
    assert!(matches!(
        register_font_validated(CARLITO, Some(bad_index)),
        Err(OfficeError::Unsupported {
            code: UnsupportedCode::FontFormatUnsupported,
            ..
        })
    ));
    assert!(matches!(
        register_font_validated(b"wOFFinvalid", None),
        Err(OfficeError::Unsupported {
            code: UnsupportedCode::FontFormatUnsupported,
            ..
        })
    ));

    let ttc = single_face_ttc(CARLITO);
    let identity = validate_font(&ttc, None).expect("valid single-face TTC");
    assert_eq!(identity.face_count, 1);
    assert_eq!(identity.face_index, 0);
}

fn single_face_ttc(sfnt: &[u8]) -> Vec<u8> {
    const FACE_OFFSET: u32 = 16;
    let mut face = sfnt.to_vec();
    let table_count = u16::from_be_bytes([face[4], face[5]]) as usize;
    for index in 0..table_count {
        let record = 12 + index * 16;
        let offset = u32::from_be_bytes([
            face[record + 8],
            face[record + 9],
            face[record + 10],
            face[record + 11],
        ]);
        face[record + 8..record + 12].copy_from_slice(&(offset + FACE_OFFSET).to_be_bytes());
    }
    let mut ttc = b"ttcf\0\x01\0\0\0\0\0\x01\0\0\0\x10".to_vec();
    ttc.extend(face);
    ttc
}

#[test]
fn strict_mode_never_uses_the_compatible_substitution_ladder() {
    let mut fonts = FontDb::with_bundled();
    fonts.set_resolution_mode(ResolutionMode::Strict);

    let resolved = resolve(&fonts, "Calibri", 400, false);

    assert!(resolved.face.is_none());
    assert_eq!(resolved.missing.as_deref(), Some("Calibri"));
    assert!(resolved.warnings.is_empty());
}

#[test]
fn compatible_style_matching_is_reported_and_stretch_remains_part_of_exact_identity() {
    let mut fonts = FontDb::new();
    fonts.register(CARLITO.to_vec()).expect("register Carlito");

    let resolved = resolve(&fonts, "Carlito", 700, false);
    assert!(resolved.face.is_some());
    assert!(
        resolved
            .warnings
            .iter()
            .any(|warning| warning.code == "font_style_compatible")
    );
    assert!(resolve_exact_with_stretch(&fonts, "Carlito", 400, false, 5).is_some());
    assert!(resolve_exact_with_stretch(&fonts, "Carlito", 400, false, 3).is_none());
}

#[test]
fn strict_profile_lists_every_unresolved_face_in_one_structured_reason() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr><w:t>First</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Ubuntu"/></w:rPr><w:t>Second</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions {
        resolution_mode: ResolutionMode::Strict,
        ..ConvertOptions::default()
    };

    let profile = profile_with_options(&bytes, OfficeFormat::Docx, &options).expect("profile");

    let reason = profile
        .reasons
        .iter()
        .find(|reason| reason.code == UnsupportedCode::FontUnresolved)
        .expect("strict unresolved reason");
    assert!(reason.detail.contains("Calibri"));
    assert!(reason.detail.contains("Ubuntu"));
    assert_eq!(profile.stats.fonts_missing, ["Calibri", "Ubuntu"]);
}

#[test]
fn strict_profile_lists_distinct_unresolved_styles_of_one_family() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Missing Face"/></w:rPr><w:t>Regular</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Missing Face"/><w:b w:val="1"/></w:rPr><w:t>Bold</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions {
        resolution_mode: ResolutionMode::Strict,
        ..ConvertOptions::default()
    };

    let profile = profile_with_options(&bytes, OfficeFormat::Docx, &options).expect("profile");
    let detail = &profile
        .reasons
        .iter()
        .find(|reason| reason.code == UnsupportedCode::FontUnresolved)
        .expect("strict unresolved reason")
        .detail;

    assert!(detail.contains("Missing Face weight=400"));
    assert!(detail.contains("Missing Face weight=700"));
}

#[test]
fn docx_font_preflight_is_style_aware_deduplicated_and_contains_no_text() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{}><w:body><w:p><w:r><w:rPr><w:rFonts w:ascii="Body Sans" w:hAnsi="Body Sans" w:eastAsia="CJK Sans"/></w:rPr><w:t>Secret Latin</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Body Sans"/><w:b/></w:rPr><w:t>Bold Latin</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Body Sans" w:eastAsia="CJK Sans"/><w:b/></w:rPr><w:t>日本</w:t></w:r></w:p></w:body></w:document>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let font_table = format!(
        r#"<?xml version="1.0"?><w:fonts{}><w:font w:name="Body Sans"><w:altName w:val="BodySans Alt"/><w:panose1 w:val="020B0604020202020204"/><w:family w:val="swiss"/><w:pitch w:val="variable"/><w:charset w:val="00"/><w:sig w:usb0="00000001" w:usb1="00000000" w:usb2="00000000" w:usb3="00000000"/></w:font><w:font w:name="CJK Sans"><w:family w:val="swiss"/></w:font></w:fonts>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let bytes = office_core::testkit::zip_package(&[
        ("[Content_Types].xml", br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
        ("_rels/.rels", br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ("word/document.xml", document.as_bytes()),
        ("word/fontTable.xml", font_table.as_bytes()),
    ]);

    let requests = font_requests(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("font preflight");

    assert_eq!(requests.len(), 3);
    let body = requests
        .iter()
        .find(|request| request.family == "Body Sans")
        .expect("Latin request");
    assert_eq!(body.weight, 400);
    assert_eq!(body.alt_families, ["BodySans Alt"]);
    assert_eq!(body.panose.as_deref(), Some("020B0604020202020204"));
    assert_eq!(body.family_class.as_deref(), Some("swiss"));
    assert_eq!(body.pitch.as_deref(), Some("variable"));
    assert_eq!(body.charset.as_deref(), Some("00"));
    assert!(body.slots.iter().any(|slot| slot.as_str() == "ascii"));
    assert!(
        requests
            .iter()
            .any(|request| request.family == "Body Sans" && request.weight == 700)
    );

    let cjk = requests
        .iter()
        .find(|request| request.family == "CJK Sans")
        .expect("East Asian request");
    assert_eq!(cjk.weight, 700);
    assert!(cjk.slots.iter().any(|slot| slot.as_str() == "east_asia"));
    assert!(cjk.scripts.iter().any(|script| script == "cjk"));

    let json = serde_json::to_string(&requests).unwrap();
    assert!(!json.contains("Secret"));
    assert!(!json.contains("Latin"));
}

#[test]
fn docx_font_preflight_covers_every_painted_story_and_excludes_comments() {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{}><w:body>
        <w:p><w:r><w:rPr><w:rFonts w:ascii="Body Face"/></w:rPr><w:t>Body</w:t></w:r></w:p>
        <w:p><w:hyperlink w:anchor="target"><w:r><w:rPr><w:rFonts w:ascii="Hyper Face"/></w:rPr><w:t>Link</w:t></w:r></w:hyperlink></w:p>
        <w:p><w:fldSimple w:instr=" PAGE "><w:r><w:rPr><w:rFonts w:ascii="Field Face"/></w:rPr><w:t>1</w:t></w:r></w:fldSimple></w:p>
        <w:tbl><w:tr><w:tc><w:p><w:r><w:rPr><w:rFonts w:ascii="Cell Face"/></w:rPr><w:t>Cell</w:t></w:r></w:p>
        <w:tbl><w:tr><w:tc><w:p><w:r><w:rPr><w:rFonts w:ascii="Nested Face"/></w:rPr><w:t>Nested</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        </w:tc></w:tr></w:tbl>
        <w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="457200"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:txbx><w:txbxContent><w:p><w:r><w:rPr><w:rFonts w:ascii="Box Face"/></w:rPr><w:t>Box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
        <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Body Face"/></w:rPr><w:t>List</w:t></w:r></w:p>
        <w:p><w:r><w:rPr><w:rFonts w:ascii="Body Face"/></w:rPr><w:footnoteReference w:id="1"/><w:endnoteReference w:id="2"/></w:r></w:p>
        <w:sectPr><w:headerReference w:type="default" r:id="rHeader"/><w:footerReference w:type="default" r:id="rFooter"/></w:sectPr>
        </w:body></w:document>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let story = |root: &str, family: &str, text: &str| {
        format!(
            r#"<?xml version="1.0"?><w:{root}{}><w:p><w:r><w:rPr><w:rFonts w:ascii="{family}"/></w:rPr><w:t>{text}</w:t></w:r></w:p></w:{root}>"#,
            office_core::testkit::DOC_NAMESPACES,
        )
    };
    let footnotes = format!(
        r#"<w:footnotes{}><w:footnote w:id="1"><w:p><w:r><w:rPr><w:rFonts w:ascii="Foot Face"/></w:rPr><w:t>Foot</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let endnotes = format!(
        r#"<w:endnotes{}><w:endnote w:id="2"><w:p><w:r><w:rPr><w:rFonts w:ascii="End Face"/></w:rPr><w:t>End</w:t></w:r></w:p></w:endnote></w:endnotes>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let numbering = format!(
        r#"<w:numbering{}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:rPr><w:rFonts w:ascii="Number Face"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let comments = format!(
        r#"<w:comments{}><w:comment w:id="0"><w:p><w:r><w:rPr><w:rFonts w:ascii="Comment Face"/></w:rPr><w:t>Not painted</w:t></w:r></w:p></w:comment></w:comments>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let root_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rFooter" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;
    let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let header = story("hdr", "Header Face", "Header");
    let footer = story("ftr", "Footer Face", "Footer");
    let bytes = office_core::testkit::zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", document_rels),
        ("word/header1.xml", header.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
        ("word/footnotes.xml", footnotes.as_bytes()),
        ("word/endnotes.xml", endnotes.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/comments.xml", comments.as_bytes()),
    ]);

    let requests = font_requests(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("all-story preflight");
    let families: std::collections::BTreeSet<_> = requests
        .iter()
        .map(|request| request.family.as_str())
        .collect();
    for family in [
        "Body Face",
        "Hyper Face",
        "Field Face",
        "Cell Face",
        "Nested Face",
        "Box Face",
        "Header Face",
        "Footer Face",
        "Foot Face",
        "End Face",
        "Number Face",
    ] {
        assert!(
            families.contains(family),
            "missing request for {family}: {families:?}"
        );
    }
    assert!(!families.contains("Comment Face"));
}

#[test]
fn docx_font_preflight_uses_the_layout_style_and_theme_cascade() {
    let document = format!(
        r#"<w:document{}><w:body>
        <w:p><w:pPr><w:pStyle w:val="Styled"/></w:pPr><w:r><w:t>Styled</w:t></w:r></w:p>
        <w:p><w:r><w:rPr><w:rFonts w:asciiTheme="minorHAnsi"/></w:rPr><w:t>Theme</w:t></w:r></w:p>
        <w:p><w:r><w:rPr><w:rFonts w:eastAsiaTheme="minorEastAsia"/></w:rPr><w:t>日本</w:t></w:r></w:p>
        <w:p><w:r><w:rPr><w:rFonts w:csTheme="minorBidi"/></w:rPr><w:t>ش</w:t></w:r></w:p>
        </w:body></w:document>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let styles = format!(
        r#"<w:styles{}><w:style w:type="paragraph" w:styleId="Styled"><w:rPr><w:rFonts w:ascii="Style Face"/></w:rPr></w:style></w:styles>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:fontScheme name="Fixture"><a:majorFont><a:latin typeface="Major Face"/></a:majorFont><a:minorFont><a:latin typeface="Theme Face"/><a:ea typeface="CJK Theme Face"/><a:cs typeface="CS Theme Face"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#;
    let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = office_core::testkit::zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/theme/theme1.xml", theme.as_bytes()),
    ]);

    let requests = font_requests(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("cascade preflight");
    let families: std::collections::BTreeSet<_> = requests
        .iter()
        .map(|request| request.family.as_str())
        .collect();
    for family in [
        "Style Face",
        "Theme Face",
        "CJK Theme Face",
        "CS Theme Face",
    ] {
        assert!(
            families.contains(family),
            "missing cascade face {family}: {families:?}"
        );
    }
}

#[test]
fn derived_theme_font_replaces_base_direct_font_in_the_same_script_slot() {
    let document = format!(
        r#"<w:document{}><w:body><w:p><w:pPr><w:pStyle w:val="Heading"/></w:pPr><w:r><w:t>Theme heading</w:t></w:r></w:p></w:body></w:document>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let styles = format!(
        r#"<w:styles{}><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading"><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/></w:rPr></w:style></w:styles>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let theme = br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:fontScheme name="Fixture"><a:majorFont><a:latin typeface="Calibri"/></a:majorFont><a:minorFont><a:latin typeface="Arial"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#;
    let content_types = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let bytes = office_core::testkit::zip_package(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/theme/theme1.xml", theme),
    ]);

    let requests = font_requests(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("derived theme font preflight");
    let families: std::collections::BTreeSet<_> = requests
        .iter()
        .map(|request| request.family.as_str())
        .collect();

    // MS-OI29500 §2.1.87: at different hierarchy levels, asciiTheme
    // overrides an inherited ascii value (and vice versa) for the ASCII slot.
    assert_eq!(families, std::collections::BTreeSet::from(["Calibri"]));
}

#[test]
fn pptx_and_xlsx_preflight_emit_deduplicated_canonical_faces() {
    for (name, bytes, format) in [
        (
            "pptx",
            include_bytes!("../../mock-files/pptx/file1.pptx").as_slice(),
            OfficeFormat::Pptx,
        ),
        (
            "xlsx",
            include_bytes!("../../mock-files/xlsx/file1.xlsx").as_slice(),
            OfficeFormat::Xlsx,
        ),
    ] {
        let requests = font_requests(bytes, format, &ConvertOptions::default())
            .unwrap_or_else(|error| panic!("{name} preflight: {error}"));
        assert!(!requests.is_empty(), "{name} emitted no font requests");
        let identities: std::collections::BTreeSet<_> = requests
            .iter()
            .map(|request| {
                (
                    request.family.to_ascii_lowercase(),
                    request.weight,
                    request.italic,
                    request.stretch,
                )
            })
            .collect();
        assert_eq!(identities.len(), requests.len(), "{name} request dedupe");
        assert!(requests.iter().all(|request| !request.slots.is_empty()));
    }
}

#[test]
fn preflight_omits_registered_exact_faces() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Carlito"/></w:rPr><w:t>Regular</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Carlito"/><w:b/></w:rPr><w:t>Bold</w:t></w:r></w:p>"#,
    );
    let regular_only = ConvertOptions {
        font_bytes: vec![CARLITO.to_vec()],
        ..ConvertOptions::default()
    };

    let requests = font_requests(&bytes, OfficeFormat::Docx, &regular_only).expect("preflight");

    assert!(requests.is_empty());
}

#[test]
fn preflight_never_requests_a_document_embedded_style() {
    let document = format!(
        r#"<w:document{}><w:body><w:p><w:r><w:rPr><w:rFonts w:ascii="Embedded Face"/></w:rPr><w:t>Visible</w:t></w:r></w:p></w:body></w:document>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let table = format!(
        r#"<w:fonts{}><w:font w:name="Embedded Face"><w:embedRegular r:id="fontRel" w:fontKey="{{00000000-0000-0000-0000-000000000000}}"/></w:font></w:fonts>"#,
        office_core::testkit::DOC_NAMESPACES,
    );
    let bytes = office_core::testkit::zip_package(&[
        ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
        ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ("word/document.xml", document.as_bytes()),
        ("word/fontTable.xml", table.as_bytes()),
    ]);

    let requests = font_requests(&bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect("embedded-font preflight");

    assert!(requests.is_empty());
}

#[test]
fn profile_and_convert_expose_the_same_font_resolution_report() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr><w:t>Report</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions::default();

    let profile = profile_with_options(&bytes, OfficeFormat::Docx, &options).expect("profile");
    let output = convert(&bytes, OfficeFormat::Docx, &options).expect("convert");

    assert_eq!(profile.fonts, output.fonts);
    assert_eq!(profile.fonts.len(), 1);
    assert_eq!(profile.fonts[0].requested_family, "Calibri");
    assert_eq!(profile.fonts[0].exactness, FontExactness::Fallback);
    assert_eq!(
        profile.fonts[0].event,
        FontResolutionEvent::FontFallbackUsed
    );
    assert!(profile.fonts[0].selected.is_some());
    assert!(profile.fonts[0].coverage_ok);
}

#[test]
fn digest_pinned_exact_face_is_not_reported_as_document_exact() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Carlito"/></w:rPr><w:t>Pinned</w:t></w:r></w:p>"#,
    );
    let digest = format!("{:x}", Sha256::digest(CARLITO));
    let options = ConvertOptions {
        font_bytes: vec![CARLITO.to_vec()],
        font_hash_pins: vec![digest],
        resolution_mode: ResolutionMode::Strict,
        ..ConvertOptions::default()
    };

    let profile = profile_with_options(&bytes, OfficeFormat::Docx, &options).expect("profile");

    assert!(profile.supported);
    assert_eq!(profile.fonts[0].exactness, FontExactness::HashPinnedExact);
}

#[test]
fn exact_registered_face_drives_pdf_font_selection_without_fallback_or_tj_arrays() {
    let bytes = office_core::testkit::minimal_docx(
        r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Carlito"/></w:rPr><w:t>Selected face</w:t></w:r></w:p>"#,
    );
    let options = ConvertOptions {
        font_bytes: vec![CARLITO.to_vec()],
        resolution_mode: ResolutionMode::Strict,
        ..ConvertOptions::default()
    };

    let output = convert(&bytes, OfficeFormat::Docx, &options).expect("exact conversion");
    assert_eq!(output.fonts.len(), 1);
    assert_eq!(
        output.fonts[0]
            .selected
            .as_ref()
            .map(|face| face.family.as_str()),
        Some("Carlito")
    );
    assert_ne!(output.fonts[0].exactness, FontExactness::Fallback);
    assert!(output.warnings.iter().all(|warning| !matches!(
        warning.code,
        "font_substituted" | "font_fallback" | "font_style_compatible"
    )));
    assert!(contains_bytes(&output.pdf, b"/ToUnicode"));
    assert!(contains_bytes(&output.pdf, b"/W ["));
    assert!(!contains_bytes(&output.pdf, b"] TJ"));
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
