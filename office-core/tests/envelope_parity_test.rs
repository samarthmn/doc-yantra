use std::path::{Path, PathBuf};

use office_core::{
    ConvertOptions, DocProfile, OfficeError, OfficeFormat, UnsupportedCode, convert, profile,
};

fn collect_docx_files(root: &Path, files: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("read directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_docx_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "docx")
        {
            files.push(path);
        }
    }
}

fn mapped_reason_code(error: &OfficeError) -> UnsupportedCode {
    DocProfile::from_error(OfficeFormat::Docx, error)
        .reasons
        .first()
        .expect("production error mapping must emit one reason")
        .code
}

fn profiled_verdict(bytes: &[u8]) -> DocProfile {
    match profile(bytes, OfficeFormat::Docx) {
        Ok(profile) => profile,
        Err(error) => DocProfile::from_error(OfficeFormat::Docx, &error),
    }
}

#[test]
fn production_error_mapping_covers_every_office_error_variant() {
    for (error, expected) in [
        (
            OfficeError::Unsupported {
                code: UnsupportedCode::MissingGlyph,
                detail: "unsupported".into(),
            },
            UnsupportedCode::MissingGlyph,
        ),
        (
            OfficeError::Invalid("invalid".into()),
            UnsupportedCode::InvalidFile,
        ),
        (
            OfficeError::Font("font".into()),
            UnsupportedCode::UnknownElement,
        ),
        (
            OfficeError::Internal("internal".into()),
            UnsupportedCode::InvalidFile,
        ),
    ] {
        assert_eq!(mapped_reason_code(&error), expected, "{error:?}");
    }
}

#[test]
fn invalid_container_profile_and_convert_use_the_same_mapped_reason() {
    let bytes = b"not a DOCX package";
    let predicted = profiled_verdict(bytes);
    let converted = convert(bytes, OfficeFormat::Docx, &ConvertOptions::default())
        .expect_err("invalid bytes must not convert");

    assert!(!predicted.supported);
    assert_eq!(predicted.reasons[0].code, mapped_reason_code(&converted));
}

#[test]
fn profile_and_convert_agree_for_every_committed_docx_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    collect_docx_files(&root.join("fidelity-corpus/docx"), &mut files);
    collect_docx_files(&root.join("mock-files"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "DOCX parity sweep found no fixtures");

    for path in files {
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let predicted = profiled_verdict(&bytes);
        let converted = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default());

        assert_eq!(
            predicted.supported,
            converted.is_ok(),
            "profile/convert support mismatch for {}: reasons={:?}, convert={:?}",
            path.display(),
            predicted.reasons,
            converted.as_ref().err()
        );
        if let Err(error) = converted {
            let first = predicted.reasons.first().unwrap_or_else(|| {
                panic!(
                    "{} conversion failed with {error:?} but profile had no reason",
                    path.display(),
                )
            });
            assert_eq!(
                first.code,
                mapped_reason_code(&error),
                "first profile reason differs from mapped convert code for {}: reasons={:?}, convert_error={error:?}",
                path.display(),
                predicted.reasons
            );
        }
    }
}
