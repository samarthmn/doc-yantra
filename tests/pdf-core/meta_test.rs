//! Integration tests for src/meta.rs — metadata, validation, optimization.

#![cfg(feature = "pdfium")]

use lopdf::{Document, LoadOptions, Object, StringFormat, dictionary};
use pdf_core::PdfCoreError;
use pdf_core::meta::{self, PdfMetadata};
use pdf_core::testkit::sample_pdf;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A structurally valid PDF whose page tree has zero pages.
fn zero_page_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

fn full_metadata() -> PdfMetadata {
    PdfMetadata {
        title: Some("Quarterly Report".to_string()),
        author: Some("Ada Lovelace".to_string()),
        subject: Some("Finance".to_string()),
        keywords: Some("pdf, rust, metadata".to_string()),
        creator: Some("doc-yantra".to_string()),
        producer: Some("pdf-core 0.1".to_string()),
    }
}

// ---------------------------------------------------------------------------
// page_count
// ---------------------------------------------------------------------------

#[test]
fn page_count_single_page() {
    let pdf = sample_pdf(&["only page"]);
    assert_eq!(meta::page_count(&pdf).unwrap(), 1);
}

#[test]
fn page_count_three_pages() {
    let pdf = sample_pdf(&["one", "two", "three"]);
    assert_eq!(meta::page_count(&pdf).unwrap(), 3);
}

#[test]
fn page_count_corrupt_bytes_errors() {
    assert!(meta::page_count(b"this is definitely not a pdf").is_err());
    assert!(meta::page_count(&[]).is_err());
}

#[test]
fn page_count_zero_pages_is_invalid_input() {
    let err = meta::page_count(&zero_page_pdf()).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

#[test]
fn validate_reports_fields_on_fixture() {
    let pdf = sample_pdf(&["a", "b", "c"]);
    let info = meta::validate(&pdf).unwrap();
    assert_eq!(info.page_count, 3);
    assert!(!info.version.is_empty());
    assert_eq!(info.version, "1.7");
    assert!(!info.encrypted);
}

#[test]
fn validate_corrupt_bytes_errors() {
    assert!(meta::validate(b"%PDF-garbage without structure").is_err());
}

#[test]
fn validate_zero_pages_errors() {
    let err = meta::validate(&zero_page_pdf()).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// page_sizes
// ---------------------------------------------------------------------------

#[test]
fn page_sizes_letter_for_all_pages() {
    let pdf = sample_pdf(&["p1", "p2", "p3", "p4"]);
    let sizes = meta::page_sizes(&pdf).unwrap();
    assert_eq!(sizes, vec![(612.0, 792.0); 4]);
}

#[test]
fn page_sizes_resolves_inherited_media_box() {
    // Build a PDF where the page has NO MediaBox and it lives on the Pages node.
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();

    let sizes = meta::page_sizes(&buf).unwrap();
    assert_eq!(sizes, vec![(595.0, 842.0)]);
}

#[test]
fn page_sizes_corrupt_bytes_errors() {
    assert!(meta::page_sizes(b"nope").is_err());
}

// ---------------------------------------------------------------------------
// get_metadata / set_metadata
// ---------------------------------------------------------------------------

#[test]
fn metadata_missing_info_dict_gives_all_none() {
    let pdf = sample_pdf(&["no info here"]);
    assert_eq!(meta::get_metadata(&pdf).unwrap(), PdfMetadata::default());
}

#[test]
fn metadata_roundtrip_all_fields() {
    let pdf = sample_pdf(&["page"]);
    let wanted = full_metadata();
    let out = meta::set_metadata(&pdf, &wanted).unwrap();

    // Read back through our API: exact match.
    assert_eq!(meta::get_metadata(&out).unwrap(), wanted);

    // Independently verify with lopdf: trailer /Info exists and the document
    // is still a valid 1-page PDF.
    let doc = Document::load_mem(&out).unwrap();
    assert!(doc.trailer.get(b"Info").is_ok());
    assert_eq!(doc.get_pages().len(), 1);
}

#[test]
fn set_metadata_none_removes_previously_set_field() {
    let pdf = sample_pdf(&["page"]);
    let first = full_metadata();
    let with_all = meta::set_metadata(&pdf, &first).unwrap();

    let second = PdfMetadata {
        title: Some("Updated Title".to_string()),
        author: None, // must remove the author set above
        subject: None,
        ..first.clone()
    };
    let out = meta::set_metadata(&with_all, &second).unwrap();
    let read = meta::get_metadata(&out).unwrap();

    assert_eq!(read.title.as_deref(), Some("Updated Title"));
    assert_eq!(read.author, None);
    assert_eq!(read.subject, None);
    assert_eq!(read.keywords, first.keywords);
    assert_eq!(read.creator, first.creator);
    assert_eq!(read.producer, first.producer);

    // The raw Info dictionary really lost the keys (not just our reader).
    let doc = Document::load_mem(&out).unwrap();
    let info_obj = doc.trailer.get(b"Info").unwrap();
    let (_, resolved) = doc.dereference(info_obj).unwrap();
    let info = resolved.as_dict().unwrap();
    assert!(!info.has(b"Author"));
    assert!(!info.has(b"Subject"));
    assert!(info.has(b"Title"));
}

#[test]
fn set_metadata_non_ascii_roundtrip() {
    let pdf = sample_pdf(&["page"]);
    let wanted = PdfMetadata {
        title: Some("Résumé — naïve façade".to_string()),
        ..PdfMetadata::default()
    };
    let out = meta::set_metadata(&pdf, &wanted).unwrap();
    assert_eq!(
        meta::get_metadata(&out).unwrap().title.as_deref(),
        Some("Résumé — naïve façade")
    );
}

#[test]
fn get_metadata_decodes_utf16be_bom_strings() {
    // Hand-build an Info dict whose Title is UTF-16BE with a BOM.
    let pdf = sample_pdf(&["page"]);
    let mut doc = Document::load_mem(&pdf).unwrap();
    let title = "Unicode ✓ Title";
    let mut bytes = vec![0xFE, 0xFF];
    for unit in title.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::String(bytes, StringFormat::Hexadecimal),
    });
    doc.trailer.set("Info", Object::Reference(info_id));
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();

    let read = meta::get_metadata(&buf).unwrap();
    assert_eq!(read.title.as_deref(), Some(title));
}

#[test]
fn get_metadata_corrupt_bytes_errors() {
    assert!(meta::get_metadata(b"not a pdf").is_err());
    assert!(meta::set_metadata(b"not a pdf", &PdfMetadata::default()).is_err());
}

// ---------------------------------------------------------------------------
// compress
// ---------------------------------------------------------------------------

#[test]
fn compress_shrinks_and_preserves_content() {
    // Long, repetitive text so the flate filter actually kicks in (lopdf only
    // applies it when the compressed stream is smaller than the original).
    let long_text = "compressible content block ".repeat(40);
    let pdf = sample_pdf(&[long_text.as_str(), "second page"]);
    let out = meta::compress(&pdf).unwrap();

    assert!(
        out.len() < pdf.len(),
        "expected shrink: {} -> {}",
        pdf.len(),
        out.len()
    );

    // Reload: identical page count, and the original text is still in the
    // (decompressed) content stream.
    let doc = Document::load_mem(&out).unwrap();
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 2);
    let p1 = doc.get_page_content(pages[&1]).unwrap();
    assert!(contains(&p1, long_text.as_bytes()));
    let p2 = doc.get_page_content(pages[&2]).unwrap();
    assert!(contains(&p2, b"second page"));

    // The raw file must no longer carry the plaintext (i.e. it was really
    // compressed, not just copied through).
    assert!(!contains(&out, long_text.as_bytes()));
}

#[test]
fn compress_tiny_doc_is_lossless_and_not_larger() {
    // Tiny streams do not compress (lopdf skips streams that would grow), so
    // assert reload-equivalence and that the size did not increase.
    let pdf = sample_pdf(&["hi"]);
    let out = meta::compress(&pdf).unwrap();
    assert!(out.len() <= pdf.len());

    let doc = Document::load_mem(&out).unwrap();
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 1);
    let content = doc.get_page_content(pages[&1]).unwrap();
    assert!(contains(&content, b"hi"));
}

#[test]
fn compress_prunes_unreferenced_objects() {
    // Add an orphan object nobody references, then compress: it must be gone.
    let pdf = sample_pdf(&["page"]);
    let mut doc = Document::load_mem(&pdf).unwrap();
    let before = doc.objects.len();
    doc.add_object(dictionary! { "Orphan" => "Yes" });
    let mut with_orphan = Vec::new();
    doc.save_to(&mut with_orphan).unwrap();

    let out = meta::compress(&with_orphan).unwrap();
    let cleaned = Document::load_mem(&out).unwrap();
    assert_eq!(cleaned.objects.len(), before);
    assert_eq!(cleaned.get_pages().len(), 1);
}

#[test]
fn compress_corrupt_bytes_errors() {
    assert!(meta::compress(b"junk").is_err());
}

// ---------------------------------------------------------------------------
// encrypt (bonus: lopdf 0.42 has real write-encryption support)
// ---------------------------------------------------------------------------

#[test]
fn encrypt_output_opens_with_password() {
    let pdf = sample_pdf(&["secret alpha", "secret beta"]);
    let enc = meta::encrypt(&pdf, "user-pw", "owner-pw").unwrap();

    let doc = Document::load_mem_with_options(&enc, LoadOptions::with_password("user-pw")).unwrap();
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 2);

    // Content streams decrypt back to the original text.
    let p1 = doc.get_page_content(pages[&1]).unwrap();
    assert!(contains(&p1, b"secret alpha"));

    // The raw encrypted bytes must not leak the plaintext.
    assert!(!contains(&enc, b"secret alpha"));
}

#[test]
fn encrypt_output_fails_without_password() {
    let pdf = sample_pdf(&["classified"]);
    let enc = meta::encrypt(&pdf, "user-pw", "owner-pw").unwrap();

    // Without a password lopdf either refuses to load or loads an empty
    // shell with no reachable pages; both are "access denied" for us.
    if let Ok(doc) = Document::load_mem(&enc) {
        assert_eq!(doc.get_pages().len(), 0)
    }
    assert!(meta::page_count(&enc).is_err());

    // Wrong password is rejected outright.
    assert!(Document::load_mem_with_options(&enc, LoadOptions::with_password("wrong-pw")).is_err());

    // validate() maps the unreadable encrypted doc to a typed PasswordProtected.
    let err = meta::validate(&enc).unwrap_err();
    assert!(
        matches!(err, PdfCoreError::PasswordProtected(_)),
        "got {err:?}"
    );
}

#[test]
fn encrypt_already_encrypted_errors() {
    let pdf = sample_pdf(&["once"]);
    let enc = meta::encrypt(&pdf, "u", "o").unwrap();
    assert!(meta::encrypt(&enc, "u2", "o2").is_err());
}

// ---------------------------------------------------------------------------
// Encrypted-input guards (regressions: compress/set_metadata destroyed
// password-encrypted docs and silently stripped owner-locked encryption)
// ---------------------------------------------------------------------------

#[test]
fn compress_and_set_metadata_refuse_password_encrypted_pdf() {
    let enc = meta::encrypt(
        &sample_pdf(&["secret alpha", "secret beta"]),
        "user-pw",
        "owner-pw",
    )
    .unwrap();

    let err = meta::compress(&enc).unwrap_err();
    assert!(matches!(err, PdfCoreError::Unsupported(_)), "got {err:?}");
    assert!(err.to_string().contains("encrypted"), "got {err}");

    let err = meta::set_metadata(&enc, &full_metadata()).unwrap_err();
    assert!(matches!(err, PdfCoreError::Unsupported(_)), "got {err:?}");
    assert!(err.to_string().contains("encrypted"), "got {err}");
}

#[test]
fn compress_and_set_metadata_refuse_owner_locked_pdf() {
    // Empty user password = "owner-locked": readers open it without a
    // password, but it is still encrypted and permission-restricted. lopdf
    // transparently decrypts it at load, so a naive re-save would emit the
    // protected plaintext with /Encrypt stripped.
    let enc = meta::encrypt(&sample_pdf(&["confidential payload"]), "", "owner-only").unwrap();
    assert!(
        !contains(&enc, b"confidential payload"),
        "fixture must be genuinely encrypted"
    );

    let err = meta::compress(&enc).unwrap_err();
    assert!(matches!(err, PdfCoreError::Unsupported(_)), "got {err:?}");

    let err = meta::set_metadata(&enc, &full_metadata()).unwrap_err();
    assert!(matches!(err, PdfCoreError::Unsupported(_)), "got {err:?}");

    // Double-encrypting an owner-locked file is refused too.
    assert!(meta::encrypt(&enc, "u2", "o2").is_err());
}

#[test]
fn validate_owner_locked_reports_encrypted_true() {
    let enc = meta::encrypt(&sample_pdf(&["x"]), "", "owner-only").unwrap();
    let info = meta::validate(&enc).unwrap();
    assert_eq!(info.page_count, 1);
    assert!(
        info.encrypted,
        "owner-locked PDF must report encrypted:true"
    );
}

#[test]
fn read_ops_on_encrypted_pdf_error_cleanly() {
    // A password-encrypted doc is not "zero pages" and its metadata is not
    // "absent" — both readings must be a clear encrypted-input error.
    let with_meta = meta::set_metadata(
        &sample_pdf(&["a", "b"]),
        &PdfMetadata {
            title: Some("Hidden Title".to_string()),
            ..PdfMetadata::default()
        },
    )
    .unwrap();

    let password_protected = meta::encrypt(&with_meta, "user-pw", "owner-pw").unwrap();
    let err = meta::page_count(&password_protected).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "got {err:?}");
    assert!(err.to_string().contains("encrypted"), "got {err}");

    let err = meta::page_sizes(&password_protected).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "got {err:?}");

    let err = meta::get_metadata(&password_protected).unwrap_err();
    assert!(matches!(err, PdfCoreError::InvalidInput(_)), "got {err:?}");

    let owner_locked = meta::encrypt(&with_meta, "", "owner-only").unwrap();
    assert_eq!(meta::page_count(&owner_locked).unwrap(), 2);
    assert_eq!(meta::page_sizes(&owner_locked).unwrap().len(), 2);
    assert_eq!(
        meta::get_metadata(&owner_locked).unwrap().title.as_deref(),
        Some("Hidden Title")
    );
}

// ---------------------------------------------------------------------------
// Text-string encodings (regressions: BOM-less UTF-8 written and assumed)
// ---------------------------------------------------------------------------

#[test]
fn set_metadata_non_ascii_writes_utf16be_with_bom() {
    let wanted = "标题 – Résumé";
    let out = meta::set_metadata(
        &sample_pdf(&["page"]),
        &PdfMetadata {
            title: Some(wanted.to_string()),
            ..PdfMetadata::default()
        },
    )
    .unwrap();

    // Roundtrip through our own reader.
    assert_eq!(
        meta::get_metadata(&out).unwrap().title.as_deref(),
        Some(wanted)
    );

    // Raw bytes: UTF-16BE with the FE FF BOM (the only non-PDFDocEncoding
    // text-string form ISO 32000-1 §7.9.2.2 allows).
    let doc = Document::load_mem(&out).unwrap();
    let info_obj = doc.trailer.get(b"Info").unwrap();
    let (_, resolved) = doc.dereference(info_obj).unwrap();
    let title = resolved.as_dict().unwrap().get(b"Title").unwrap();
    let Object::String(bytes, _) = title else {
        panic!("Title is not a string: {title:?}");
    };
    assert!(bytes.starts_with(b"\xFE\xFF"), "missing UTF-16BE BOM");
    let units: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), wanted);

    // Interop: pdfium (an independent conforming reader) must see the same
    // string, not mojibake.
    let pdfium = pdf_core::pdfium::lock().expect("pdfium");
    let d = pdfium.load_pdf_from_byte_slice(&out, None).expect("load");
    let read = d
        .metadata()
        .get(pdfium_render::prelude::PdfDocumentMetadataTagType::Title)
        .map(|t| t.value().to_string());
    assert_eq!(read.as_deref(), Some(wanted));
}

#[test]
fn set_metadata_parens_and_backslash_roundtrip() {
    // ASCII with delimiters stays a (properly escaped) literal string;
    // non-ASCII with delimiters goes through the UTF-16BE path.
    for wanted in ["Report (v2) \\ draft )(", "Résumé (draft) c:\\tmp\\)x("] {
        let out = meta::set_metadata(
            &sample_pdf(&["page"]),
            &PdfMetadata {
                title: Some(wanted.to_string()),
                ..PdfMetadata::default()
            },
        )
        .unwrap();
        assert_eq!(
            meta::get_metadata(&out).unwrap().title.as_deref(),
            Some(wanted),
            "roundtrip failed for {wanted:?}"
        );
    }
}

#[test]
fn get_metadata_decodes_bomless_strings_as_pdfdocencoding() {
    // Author a BOM-less Info dict the way Word/Acrobat/LaTeX do: raw
    // PDFDocEncoding bytes. 0xE9 = é, 0x85 = en dash, 0xA0 = € (Annex D).
    let mut doc = Document::load_mem(&sample_pdf(&["page"])).unwrap();
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::String(b"R\xE9sum\xE9".to_vec(), StringFormat::Literal),
        "Author" => Object::String(b"A\x85B".to_vec(), StringFormat::Literal),
        "Subject" => Object::String(b"price \xA0 5".to_vec(), StringFormat::Literal),
    });
    doc.trailer.set("Info", Object::Reference(info_id));
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();

    let read = meta::get_metadata(&buf).unwrap();
    assert_eq!(read.title.as_deref(), Some("Résumé"));
    assert_eq!(read.author.as_deref(), Some("A\u{2013}B"));
    assert_eq!(read.subject.as_deref(), Some("price \u{20AC} 5"));
}

#[test]
fn get_metadata_decodes_utf8_bom_strings() {
    let mut doc = Document::load_mem(&sample_pdf(&["page"])).unwrap();
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend_from_slice("Résumé ✓".as_bytes());
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::String(bytes, StringFormat::Hexadecimal),
    });
    doc.trailer.set("Info", Object::Reference(info_id));
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();

    assert_eq!(
        meta::get_metadata(&buf).unwrap().title.as_deref(),
        Some("Résumé ✓")
    );
}

// ---------------------------------------------------------------------------
// Trailing-garbage tolerance (regression: >=512B after %%EOF hard-rejected)
// ---------------------------------------------------------------------------

#[test]
fn trailing_garbage_after_eof_is_tolerated() {
    // Email gateways / HTTP appends tack bytes onto real files; pdfium and
    // Acrobat still open them. lopdf's startxref scan only covers the last
    // ~512 bytes, so these inputs used to hard-fail with Xref(Start).
    let pdf = sample_pdf(&["one", "two"]);
    for n in [512usize, 1024, 4096] {
        let mut junk = pdf.clone();
        junk.push(b'\n');
        junk.extend(std::iter::repeat_n(b'G', n));

        let info = meta::validate(&junk).unwrap_or_else(|e| panic!("{n}B garbage: {e:?}"));
        assert_eq!(info.page_count, 2, "{n}B garbage");
        assert!(!info.encrypted);
        assert_eq!(meta::page_count(&junk).unwrap(), 2, "{n}B garbage");
        assert!(meta::get_metadata(&junk).is_ok(), "{n}B garbage");

        // Write paths use the same tolerant loader and emit a clean file.
        let out = meta::compress(&junk).unwrap_or_else(|e| panic!("{n}B garbage: {e:?}"));
        assert_eq!(Document::load_mem(&out).unwrap().get_pages().len(), 2);
    }
}

#[test]
fn truly_corrupt_input_still_errors_despite_eof_retry() {
    // A %%EOF marker alone must not make garbage parse.
    assert!(meta::validate(b"not a pdf %%EOF more junk").is_err());
    assert!(meta::page_count(b"%%EOF").is_err());
}

// ---------------------------------------------------------------------------
// MediaBox normalization (regression: negative width/height)
// ---------------------------------------------------------------------------

#[test]
fn page_sizes_normalizes_swapped_media_box_corners() {
    // ISO 32000-1 §7.9.5: rectangle corners may be written in any order.
    let mut doc = Document::load_mem(&sample_pdf(&["x"])).unwrap();
    let pid = *doc.get_pages().values().next().unwrap();
    doc.get_dictionary_mut(pid).unwrap().set(
        "MediaBox",
        vec![
            Object::from(612),
            Object::from(792),
            Object::from(0),
            Object::from(0),
        ],
    );
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();

    assert_eq!(meta::page_sizes(&buf).unwrap(), vec![(612.0, 792.0)]);
}
