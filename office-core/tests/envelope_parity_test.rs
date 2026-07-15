use std::path::{Path, PathBuf};

use office_core::{ConvertOptions, OfficeError, OfficeFormat, convert, profile};

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
        let predicted = profile(&bytes, OfficeFormat::Docx)
            .unwrap_or_else(|error| panic!("profile {}: {error}", path.display()));
        let converted = convert(&bytes, OfficeFormat::Docx, &ConvertOptions::default());

        assert_eq!(
            predicted.supported,
            converted.is_ok(),
            "profile/convert support mismatch for {}: reasons={:?}, convert={:?}",
            path.display(),
            predicted.reasons,
            converted.as_ref().err()
        );
        if let Err(OfficeError::Unsupported { code, detail }) = converted {
            let first = predicted.reasons.first().unwrap_or_else(|| {
                panic!(
                    "{} converted with unsupported:{code:?}:{detail} but profile had no reason",
                    path.display()
                )
            });
            assert_eq!(
                first.code,
                code,
                "first profile reason differs from convert code for {}: reasons={:?}, convert_detail={detail}",
                path.display(),
                predicted.reasons
            );
        }
    }
}
