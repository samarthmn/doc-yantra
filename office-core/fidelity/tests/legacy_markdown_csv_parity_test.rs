//! Per-page semantic parity against frozen Chromium markdown/CSV goldens.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fidelity::evidence::verify_sha256;
use pdf_core::embed_font::register_font;
use pdf_core::extract::extract_text_pages;
use pdf_core::generate::{DocOptions, csv_to_pdf, markdown_to_pdf};
use pdf_core::meta::validate;
use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    goldens: Vec<GoldenRow>,
}

#[derive(Deserialize)]
struct GoldenRow {
    input_path: String,
    input_sha256: String,
    golden_path: String,
    golden_sha256: String,
}

#[derive(Deserialize)]
struct DispositionRegistry {
    schema_version: u32,
    known_failures: Vec<KnownFailure>,
}

#[derive(Deserialize)]
struct KnownFailure {
    fixture_id: String,
    reason_code: String,
    justification: String,
    user_visible_effect: String,
    tracking_note: String,
}

struct Finding {
    fixture_id: String,
    reason_code: &'static str,
    detail: String,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(root: &Path, relative: &str) -> Vec<u8> {
    std::fs::read(root.join(relative)).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, relative: &str) -> T {
    serde_json::from_slice(&read(root, relative))
        .unwrap_or_else(|error| panic!("parse {relative}: {error}"))
}

fn fixture_id(golden_path: &str) -> String {
    golden_path
        .strip_prefix("fidelity-corpus/legacy-goldens/")
        .and_then(|path| path.strip_suffix(".chromium.pdf"))
        .unwrap_or_else(|| panic!("unexpected Chromium golden path: {golden_path}"))
        .to_string()
}

fn register_packaged_fonts(root: &Path) {
    for file in [
        "LiberationSans-Regular.ttf",
        "LiberationSans-Bold.ttf",
        "LiberationMono-Regular.ttf",
        "NotoSansCJKsc-Regular.otf",
        "NotoSansCJKtc-Regular.otf",
        "NotoSansCJKjp-Regular.otf",
        "NotoSansCJKkr-Regular.otf",
        "NotoSansArabic-Variable.ttf",
        "NotoSansDevanagari-Variable.ttf",
        "NotoSansThai-Variable.ttf",
        "NotoEmoji-Variable.ttf",
    ] {
        register_font(&read(
            root,
            &format!("packages/doc-yantra-fonts/fonts/{file}"),
        ))
        .unwrap_or_else(|error| panic!("register {file}: {error}"));
    }
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn first_mismatch(golden: &[&str], current: &[&str]) -> Option<usize> {
    golden
        .iter()
        .zip(current)
        .position(|(golden, current)| golden != current)
        .or_else(|| (golden.len() != current.len()).then_some(golden.len().min(current.len())))
}

fn evaluate(root: &Path, row: &GoldenRow) -> Vec<Finding> {
    let fixture_id = fixture_id(&row.golden_path);
    let input = read(root, &row.input_path);
    let golden = read(root, &row.golden_path);
    verify_sha256(&root.join(&row.input_path), &input, &row.input_sha256)
        .unwrap_or_else(|error| panic!("{error}"));
    verify_sha256(&root.join(&row.golden_path), &golden, &row.golden_sha256)
        .unwrap_or_else(|error| panic!("{error}"));

    let current = if fixture_id.starts_with("markdown/") {
        markdown_to_pdf(
            std::str::from_utf8(&input).expect("markdown input must be UTF-8"),
            &DocOptions::default(),
        )
    } else {
        csv_to_pdf(&input, &DocOptions::default())
    };
    let current = match current {
        Ok(current) => current,
        Err(error) => {
            return vec![Finding {
                fixture_id,
                reason_code: "MDCSV_CONVERSION",
                detail: error.to_string(),
            }];
        }
    };

    let golden_page_count = validate(&golden)
        .expect("frozen Chromium golden must validate")
        .page_count;
    let current_page_count = validate(&current)
        .expect("current markdown/CSV PDF must validate")
        .page_count;
    let mut findings = Vec::new();
    if current_page_count != golden_page_count {
        findings.push(Finding {
            fixture_id: fixture_id.clone(),
            reason_code: "MDCSV_PAGE_COUNT",
            detail: format!("golden={golden_page_count} current={current_page_count}"),
        });
    }

    let golden_pages = extract_text_pages(&golden).expect("extract frozen Chromium pages");
    let current_pages = extract_text_pages(&current).expect("extract current markdown/CSV pages");
    for (page_index, (golden_page, current_page)) in
        golden_pages.iter().zip(&current_pages).enumerate()
    {
        let golden_words = words(golden_page);
        let current_words = words(current_page);
        if golden_words != current_words {
            findings.push(Finding {
                fixture_id: fixture_id.clone(),
                reason_code: "MDCSV_PAGE_WORD_ORDER",
                detail: format!(
                    "page={} mismatch={:?} golden-words={} current-words={}",
                    page_index + 1,
                    first_mismatch(&golden_words, &current_words),
                    golden_words.len(),
                    current_words.len()
                ),
            });
        }
    }
    eprintln!(
        "LEGACY MD/CSV {fixture_id}: pages={golden_page_count}->{current_page_count}; compared-pages={}",
        golden_pages.len().min(current_pages.len())
    );
    findings
}

#[test]
fn every_markdown_and_csv_golden_compares_words_on_its_original_page() {
    let root = root();
    register_packaged_fonts(&root);
    let manifest: Manifest = read_json(&root, "fidelity-corpus/legacy-goldens/manifest.json");
    let registry: DispositionRegistry =
        read_json(&root, "fidelity-corpus/legacy-parity-allowlist.json");
    assert_eq!(registry.schema_version, 2, "unknown disposition schema");
    let rows: Vec<&GoldenRow> = manifest
        .goldens
        .iter()
        .filter(|row| row.golden_path.contains("/markdown/") || row.golden_path.contains("/csv/"))
        .collect();
    assert_eq!(rows.len(), 9, "markdown/CSV manifest row-count drift");

    let known: Vec<&KnownFailure> = registry
        .known_failures
        .iter()
        .filter(|entry| {
            (entry.fixture_id.starts_with("markdown/") || entry.fixture_id.starts_with("csv/"))
                && entry.reason_code.starts_with("MDCSV_")
        })
        .collect();
    for entry in &known {
        assert!(!entry.fixture_id.trim().is_empty());
        assert!(!entry.reason_code.trim().is_empty());
        assert!(!entry.justification.trim().is_empty());
        assert!(!entry.user_visible_effect.trim().is_empty());
        assert!(!entry.tracking_note.trim().is_empty());
    }
    let known_keys: BTreeSet<(&str, &str)> = known
        .iter()
        .map(|entry| (entry.fixture_id.as_str(), entry.reason_code.as_str()))
        .collect();
    assert_eq!(known_keys.len(), known.len(), "duplicate known failures");

    let mut observed = BTreeSet::new();
    let mut unexpected = Vec::new();
    let mut red_fixtures = BTreeSet::new();
    for row in rows {
        for finding in evaluate(&root, row) {
            let key = (finding.fixture_id.as_str(), finding.reason_code);
            if known_keys.contains(&key) {
                eprintln!(
                    "RED TRACKED {} {}: {}",
                    finding.fixture_id, finding.reason_code, finding.detail
                );
                red_fixtures.insert(finding.fixture_id.clone());
                observed.insert((finding.fixture_id, finding.reason_code.to_string()));
            } else {
                unexpected.push(format!(
                    "{} {}: {}",
                    finding.fixture_id, finding.reason_code, finding.detail
                ));
            }
        }
    }
    let expected: BTreeSet<(String, String)> = known_keys
        .iter()
        .map(|(fixture, reason)| ((*fixture).to_string(), (*reason).to_string()))
        .collect();
    assert_eq!(observed, expected, "markdown/CSV known failures are stale");
    assert!(
        unexpected.is_empty(),
        "untracked markdown/CSV parity reds:\n{}",
        unexpected.join("\n")
    );
    eprintln!(
        "LEGACY MD/CSV DISPOSITIONS: green={} allowlisted=0 red={}",
        9 - red_fixtures.len(),
        red_fixtures.len()
    );
}
