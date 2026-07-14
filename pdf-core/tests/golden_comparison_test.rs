//! P3.4 semantic comparison against the frozen e0ff8fb markdown/CSV goldens.
//! The oracle is read-only: this test never invokes the retired service.

#![cfg(feature = "pdfium")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lopdf::{Document, Object};
use pdf_core::embed_font::register_font;
use pdf_core::extract::extract_text;
use pdf_core::generate::{DocOptions, csv_to_pdf, markdown_to_pdf};
use serial_test::serial;

const CASES: &[(&str, &str, &str, bool, usize)] = &[
    (
        "markdown/file1",
        "mock-files/md/file1.md",
        "fidelity-corpus/legacy-goldens/markdown/file1.chromium.pdf",
        true,
        1,
    ),
    (
        "markdown/file2",
        "mock-files/md/file2.md",
        "fidelity-corpus/legacy-goldens/markdown/file2.chromium.pdf",
        true,
        1,
    ),
    (
        "markdown/unicode-mixed",
        "fidelity-corpus/legacy-goldens/inputs/unicode-mixed.md",
        "fidelity-corpus/legacy-goldens/markdown/unicode-mixed.chromium.pdf",
        true,
        2,
    ),
    (
        "markdown/gfm-constructs",
        "fidelity-corpus/legacy-goldens/inputs/gfm-constructs.md",
        "fidelity-corpus/legacy-goldens/markdown/gfm-constructs.chromium.pdf",
        true,
        2,
    ),
    (
        "markdown/long-pagination",
        "fidelity-corpus/legacy-goldens/inputs/long-pagination.md",
        "fidelity-corpus/legacy-goldens/markdown/long-pagination.chromium.pdf",
        true,
        17,
    ),
    (
        "csv/file1",
        "mock-files/csv/file1.csv",
        "fidelity-corpus/legacy-goldens/csv/file1.chromium.pdf",
        false,
        1,
    ),
    (
        "csv/file2",
        "mock-files/csv/file2.csv",
        "fidelity-corpus/legacy-goldens/csv/file2.chromium.pdf",
        false,
        1,
    ),
    (
        "csv/latin1",
        "fidelity-corpus/legacy-goldens/inputs/latin1.csv",
        "fidelity-corpus/legacy-goldens/csv/latin1.chromium.pdf",
        false,
        1,
    ),
    (
        "csv/unicode",
        "fidelity-corpus/legacy-goldens/inputs/unicode.csv",
        "fidelity-corpus/legacy-goldens/csv/unicode.chromium.pdf",
        false,
        1,
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn read(root: &Path, path: &str) -> Vec<u8> {
    std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}
fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A run is a non-whitespace line from pdfium extraction. This deliberately
/// tolerates line wrapping but makes text loss visible in the report/pin.
fn runs(value: &str) -> BTreeSet<String> {
    value
        .lines()
        .map(normalize)
        .filter(|run| !run.is_empty())
        .collect()
}
fn words(value: &str) -> Vec<&str> {
    value.split_whitespace().collect()
}
fn contains_words(haystack: &[&str], needle: &[&str]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
fn uris(pdf: &[u8]) -> BTreeSet<String> {
    let doc = Document::load_mem(pdf).expect("parse PDF");
    doc.objects
        .values()
        .filter_map(|object| {
            let dict = object.as_dict().ok()?;
            if !matches!(dict.get(b"S"), Ok(Object::Name(name)) if name == b"URI") {
                return None;
            }
            match dict.get(b"URI") {
                Ok(Object::String(uri, _)) => Some(String::from_utf8_lossy(uri).into_owned()),
                _ => None,
            }
        })
        .collect()
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
        register_font(&read(root, &format!("pdf-core/wasm/pkg/fonts/{file}")))
            .unwrap_or_else(|e| panic!("register {file}: {e}"));
    }
}

#[test]
#[serial(golden_comparison)]
fn frozen_markdown_and_csv_goldens_have_a_committed_semantic_runner() {
    let root = root();
    register_packaged_fonts(&root);
    let mut red = Vec::new();
    for (name, input_path, golden_path, markdown, expected_pages) in CASES {
        let input = read(&root, input_path);
        let current = if *markdown {
            markdown_to_pdf(
                std::str::from_utf8(&input).expect("UTF-8 markdown"),
                &DocOptions::default(),
            )
        } else {
            csv_to_pdf(&input, &DocOptions::default())
        };
        let golden = read(&root, golden_path);
        let golden_doc = Document::load_mem(&golden).expect("golden parses");
        let old_pages = golden_doc.get_pages().len();
        let current = match current {
            Ok(pdf) => pdf,
            Err(error) => {
                eprintln!("GOLDEN {name}: pages {old_pages}->ERR; conversion={error}");
                red.push(format!("{name}: conversion failed: {error}"));
                continue;
            }
        };
        let current_doc = Document::load_mem(&current).expect("current parses");
        let new_pages = current_doc.get_pages().len();
        // Compare against the page count parsed from the frozen golden, never against a
        // constant recorded from our own output: a constant only ever proves that the
        // engine still does what it did yesterday.
        assert_eq!(
            new_pages, old_pages,
            "{name}: page count must match the frozen golden"
        );
        assert_eq!(
            old_pages, *expected_pages,
            "{name}: frozen golden page count drifted from the manifest"
        );
        let old_text = extract_text(&golden).expect("extract golden");
        let new_text = extract_text(&current).expect("extract current");
        let old_runs = runs(&old_text);
        let new_runs = runs(&new_text);
        let missing: Vec<_> = old_runs.difference(&new_runs).cloned().collect();
        let old_uris = uris(&golden);
        let new_uris = uris(&current);
        let missing_uris: Vec<_> = old_uris.difference(&new_uris).cloned().collect();
        eprintln!(
            "GOLDEN {name}: pages {old_pages}->{new_pages}; runs={}/{}; missing-uris={}",
            old_runs.len() - missing.len(),
            old_runs.len(),
            missing_uris.len()
        );
        // The frozen Chromium oracle extracts RTL/complex-script runs in visual
        // order. Per fidelity-corpus/legacy-goldens/comparison-report.md, this
        // row instead requires the engine's documented logical-order text.
        let unicode_logical_order = *name == "markdown/unicode-mixed";
        if *name == "markdown/long-pagination" {
            // Chromium and local-tier geometry wrap the same prose at different
            // widths. comparison-report.md records 8,803/8,803 word parity, so
            // exact full-document token equality is the stable content gate.
            let old_words = words(&old_text);
            let new_words = words(&new_text);
            if old_words != new_words {
                let mismatch = old_words
                    .iter()
                    .zip(&new_words)
                    .position(|(old, new)| old != new);
                red.push(format!(
                    "{name}: word sequence differs at {mismatch:?}; words={}/{}",
                    new_words.len(),
                    old_words.len()
                ));
            }
        } else if unicode_logical_order {
            for logical in [
                "Arabic — اختبار عربي",
                "هذا نص تجريبي باللغة العربية. يحتوي على أرقام عربية ٠١٢٣ وحروف متصلة.",
                "Hebrew — בדיקה",
                "זהו טקסט בדיקה בעברית. כולל ניקוד: שָׁלוֹם.",
                "Devanagari — परीक्षण",
                "यह देवनागरी में एक परीक्षण वाक्य है। संयुक्ताक्षर: क्ष त्र ज्ञ।",
                "Thai — ทดสอบ",
                "นี่คือข้อความทดสอบภาษาไทยที่ไม่มีการเว้นวรรคระหว่างคำและมีวรรณยุกต์ซ้อนกัน",
                "Total: 123 ريال",
            ] {
                if !new_text.contains(logical) {
                    red.push(format!("{name}: missing logical-order text {logical:?}"));
                }
            }
        } else if *name == "markdown/gfm-constructs" {
            // Nested-list markers and wrapped footnotes change PDFium run
            // boundaries. Require every frozen run's full word sequence in the
            // current document; the backreference arrow remains a required word.
            let new_words = words(&new_text);
            let missing_words: Vec<_> = old_runs
                .iter()
                .filter(|run| !contains_words(&new_words, &words(run)))
                .cloned()
                .collect();
            if !missing_words.is_empty() || !missing_uris.is_empty() {
                red.push(format!(
                    "{name}: word-runs={missing_words:?}; uris={missing_uris:?}"
                ));
            }
        } else if !missing.is_empty() || !missing_uris.is_empty() {
            // GFM URI parity is a hard semantic gate. Other rows retain their
            // exact frozen-run comparison without changing its thresholds.
            red.push(format!("{name}: runs={missing:?}; uris={missing_uris:?}"));
        }
        if unicode_logical_order {
            for probe in ["日本語", "中文", "한국어", "русском", "ελληνικά"]
            {
                if !new_text.contains(probe) {
                    red.push(format!("{name}: missing Unicode probe {probe}"));
                }
            }
        }
    }
    if !red.is_empty() {
        panic!("P3.4 parity red:\n{}", red.join("\n"));
    }
}
