//! CFF (OpenType/CIDFontType0C) embedding must stay renderable by pdf.js.
//!
//! pdf.js (3.11.174 and current master) resolves CIDFontType0C glyphs by
//! inverting the embedded /Encoding CMap per glyph: `CMap.charCodeOf(cid)`
//! returns the LOWEST charcode mapping to that CID, and only that charcode can
//! paint the glyph. A CMap that also lists unused lower codes for the same
//! CIDs (an identity "prelude" over the subset glyph ids) therefore blanks
//! every code the content stream actually uses — extraction and advances stay
//! correct, so nothing warns. This test locks the invertible CMap shape at the
//! byte level; the end-to-end paint proof lives in
//! tests/office-cjk-pdfjs-paint.test.mjs (repo root, pdf.js + canvas).

use office_core::testkit::{DOC_NAMESPACES, zip_package};
use std::path::{Path, PathBuf};
use std::sync::Once;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn register_noto_jp() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let noto = std::fs::read(repo_path(
            "../pdf-core/wasm/pkg/fonts/NotoSansCJKjp-Regular.otf",
        ))
        .expect("repo-committed Noto Sans CJK JP OTF");
        office_core::register_font(&noto).expect("register CJK face");
    });
}

fn cjk_docx(body_paragraphs: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0"?><w:document{DOC_NAMESPACES}><w:body>{body_paragraphs}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip_package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

const NOTO_RPR: &str =
    r#"<w:rPr><w:rFonts w:ascii="Noto Sans CJK JP" w:eastAsia="Noto Sans CJK JP"/></w:rPr>"#;

/// Semantic-cluster pairs share their cluster's owner gid without painting
/// ink; the pdf.js one-code-per-glyph gate must not reject them (regression:
/// this bidi document converted before the gate landed).
#[test]
fn cluster_mapped_cjk_text_stays_convertible() {
    register_noto_jp();
    let body =
        format!(r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r>{NOTO_RPR}<w:t>本 木</w:t></w:r></w:p>"#);
    let bytes = cjk_docx(&body);
    let output = office_core::convert(
        &bytes,
        office_core::OfficeFormat::Docx,
        &office_core::ConvertOptions::default(),
    )
    .expect("cluster-owner code aliases never paint through their codes");
    assert_eq!(output.page_count, 1);
}

/// Two visible codepoints cmap-aliased onto one inked CFF glyph genuinely
/// cannot both paint under pdf.js; convert fails closed and profile() must
/// predict the same verdict (DocProfile contract).
#[test]
fn profile_predicts_the_visible_alias_collision_verdict() {
    register_noto_jp();
    // U+4E00 and Kangxi radical U+2F00 share one glyph in Noto Sans CJK JP.
    let body = format!(r#"<w:p><w:r>{NOTO_RPR}<w:t>一&#x2F00;</w:t></w:r></w:p>"#);
    let bytes = cjk_docx(&body);
    let converted = office_core::convert(
        &bytes,
        office_core::OfficeFormat::Docx,
        &office_core::ConvertOptions::default(),
    );
    let error = converted.expect_err("visible alias collision must fail closed");
    assert!(
        error.to_string().starts_with("unsupported:"),
        "typed unsupported error: {error}"
    );
    let profile =
        office_core::profile(&bytes, office_core::OfficeFormat::Docx).expect("profile runs");
    assert!(
        !profile.supported,
        "profile must predict the emit-time collision verdict: {:?}",
        profile.reasons
    );
}

/// Every `<code> cid` pair from every begincidchar block after `start`,
/// stopping at the matching `endcmap`.
fn parse_cidchar_entries(cmap_text: &str) -> Vec<(u32, u32)> {
    let mut entries = Vec::new();
    let mut in_block = false;
    for line in cmap_text.lines() {
        let line = line.trim();
        if line.ends_with("begincidchar") {
            in_block = true;
            continue;
        }
        if line == "endcidchar" {
            in_block = false;
            continue;
        }
        if line == "endcmap" {
            break;
        }
        if in_block
            && let Some(rest) = line.strip_prefix('<')
            && let Some((hex, cid)) = rest.split_once("> ")
        {
            let code = u32::from_str_radix(hex, 16).expect("hex content code");
            let cid: u32 = cid.trim().parse().expect("decimal CID");
            entries.push((code, cid));
        }
    }
    entries
}

#[test]
fn cff_encoding_cmap_is_invertible_for_pdfjs() {
    let noto = std::fs::read(repo_path(
        "../pdf-core/wasm/pkg/fonts/NotoSansCJKjp-Regular.otf",
    ))
    .expect("repo-committed Noto Sans CJK JP OTF");
    office_core::register_font(&noto).expect("register CJK face");

    let source =
        std::fs::read(repo_path("../mock-files/cjk-docx/japanese.docx")).expect("CJK fixture");
    let output = office_core::convert(
        &source,
        office_core::OfficeFormat::Docx,
        &office_core::ConvertOptions::default(),
    )
    .expect("japanese.docx stays inside the envelope");
    assert_eq!(output.page_count, 8, "Word page parity must not regress");
    let pdf = output.pdf;

    // The emitter writes the /Encoding CMap stream uncompressed; the marker
    // appears both in the stream dictionary (/CMapName) and in the CMap
    // program itself, so group occurrences by their enclosing object.
    let marker = b"/DocYantra-CFF-Encoding";
    let mut object_starts = std::collections::BTreeSet::new();
    let mut search_from = 0;
    while let Some(found) = pdf[search_from..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let marker_at = search_from + found;
        let object_start = pdf[..marker_at]
            .windows(3)
            .rposition(|window| window == b"obj")
            .expect("CMap marker inside an object");
        object_starts.insert(object_start);
        search_from = marker_at + marker.len();
    }
    assert!(
        !object_starts.is_empty(),
        "japanese.docx must embed at least one CFF face with the custom CMap"
    );

    for object_start in object_starts {
        let object_tail = &pdf[object_start..];
        let stream_at = object_tail
            .windows(6)
            .position(|window| window == b"stream")
            .expect("CMap object carries a stream");
        let dict = String::from_utf8_lossy(&object_tail[..stream_at]);
        // PDF 32000-1 §9.7.5.3: an embedded CMap stream dictionary carries
        // /Type /CMap, /CMapName and /CIDSystemInfo.
        assert!(
            dict.contains("/Type /CMap") && dict.contains("/CMapName"),
            "embedded CMap stream dict must declare /Type /CMap and /CMapName: {dict}"
        );
        assert!(
            dict.contains("/CIDSystemInfo"),
            "embedded CMap stream dict must declare /CIDSystemInfo: {dict}"
        );

        let content = String::from_utf8_lossy(&object_tail[stream_at..]);
        let entries = parse_cidchar_entries(&content);
        assert!(
            !entries.is_empty(),
            "CFF encoding CMap lists no cidchar entries"
        );

        // Invertibility: content codes live strictly above the CID (= subset
        // glyph id) range, so no unused identity-prelude code can shadow a
        // used code under pdf.js's lowest-charcode inversion.
        let min_code = entries.iter().map(|(code, _)| *code).min().unwrap();
        let max_cid = entries.iter().map(|(_, cid)| *cid).max().unwrap();
        assert!(
            min_code > max_cid,
            "CFF /Encoding CMap still contains identity-prelude codes: \
             min code {min_code:#06x} <= max CID {max_cid} — pdf.js resolves \
             CID->charcode with lowest-wins, so these shadow every used code"
        );
    }
}
