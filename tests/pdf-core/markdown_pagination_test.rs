//! Golden-derived pagination checks for Markdown and CSV generation.

#![cfg(feature = "pdfium")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use lopdf::{Document, Object};
use pdf_core::embed_font::register_font;
use pdf_core::extract::extract_text_pages;
use pdf_core::generate::{DocOptions, PageSize, csv_to_pdf, markdown_to_pdf};
use serial_test::serial;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn has_unstable_pdfium_word_boundaries(word: &str) -> bool {
    word.chars().any(|character| {
        matches!(
            character as u32,
            0x0590..=0x08FF | 0x0900..=0x097F | 0x0E00..=0x0E7F | 0x1F000..=0x1FAFF
        ) || matches!(character, '\u{200D}' | '\u{FE0F}')
    })
}

fn word_multiset(text: &str, stable_pdfium_boundaries_only: bool) -> BTreeMap<String, usize> {
    let mut words = BTreeMap::new();
    for word in text.split_whitespace() {
        if stable_pdfium_boundaries_only && has_unstable_pdfium_word_boundaries(word) {
            continue;
        }
        let word = word.trim_matches(|character: char| !character.is_alphanumeric());
        if !word.is_empty() {
            *words.entry(word.to_string()).or_default() += 1;
        }
    }
    words
}

fn uri_targets(pdf: &[u8]) -> BTreeSet<String> {
    Document::load_mem(pdf)
        .expect("parse PDF for link targets")
        .objects
        .values()
        .filter_map(|object| {
            let dictionary = object.as_dict().ok()?;
            if !matches!(dictionary.get(b"S"), Ok(Object::Name(name)) if name == b"URI") {
                return None;
            }
            match dictionary.get(b"URI") {
                Ok(Object::String(uri, _)) => Some(String::from_utf8_lossy(uri).into_owned()),
                _ => None,
            }
        })
        .collect()
}

fn register_packaged_fonts(root: &std::path::Path) {
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
        let path = root.join("packages/doc-yantra-fonts/fonts").join(file);
        register_font(
            &std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read packaged font {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("register {file}: {error}"));
    }
}

fn assert_golden_pages_and_words(name: &str, current: &[u8], golden: &[u8]) {
    let golden_page_count = Document::load_mem(golden)
        .unwrap_or_else(|error| panic!("parse {name} golden: {error}"))
        .get_pages()
        .len();
    let current_page_count = Document::load_mem(current)
        .unwrap_or_else(|error| panic!("parse generated {name}: {error}"))
        .get_pages()
        .len();
    eprintln!("{name}: golden={golden_page_count} current={current_page_count}");
    assert_eq!(
        current_page_count, golden_page_count,
        "{name}: page count must come from the frozen golden"
    );

    let golden_pages =
        extract_text_pages(golden).unwrap_or_else(|error| panic!("extract {name} golden: {error}"));
    let current_pages = extract_text_pages(current)
        .unwrap_or_else(|error| panic!("extract generated {name}: {error}"));
    let stable_pdfium_boundaries_only = name == "markdown/unicode-mixed";
    let golden_words: Vec<_> = golden_pages
        .iter()
        .map(|page| word_multiset(page, stable_pdfium_boundaries_only))
        .collect();
    let current_words: Vec<_> = current_pages
        .iter()
        .map(|page| word_multiset(page, stable_pdfium_boundaries_only))
        .collect();
    if current_words != golden_words {
        let differences: Vec<_> = current_words
            .iter()
            .zip(&golden_words)
            .enumerate()
            .filter_map(|(page, (current, golden))| {
                let current_only: Vec<_> = current
                    .iter()
                    .filter(|(word, count)| golden.get(*word) != Some(*count))
                    .collect();
                let golden_only: Vec<_> = golden
                    .iter()
                    .filter(|(word, count)| current.get(*word) != Some(*count))
                    .collect();
                (!current_only.is_empty() || !golden_only.is_empty()).then_some((
                    page + 1,
                    current_only,
                    golden_only,
                ))
            })
            .collect();
        panic!(
            "{name}: each generated page must contain the golden page's word multiset: {differences:?}"
        );
    }
    assert_eq!(
        uri_targets(current),
        uri_targets(golden),
        "{name}: link URI targets changed"
    );
}

#[test]
fn long_markdown_matches_golden_pagination_and_page_words() {
    let root = root();
    let markdown = std::fs::read_to_string(
        root.join("tests/fixtures/corpus/legacy-goldens/inputs/long-pagination.md"),
    )
    .expect("read long Markdown fixture");
    let golden = std::fs::read(
        root.join("tests/fixtures/corpus/legacy-goldens/markdown/long-pagination.chromium.pdf"),
    )
    .expect("read long Markdown golden");
    let current =
        markdown_to_pdf(&markdown, &DocOptions::default()).expect("render long Markdown fixture");

    let golden_pages = Document::load_mem(&golden)
        .expect("parse long Markdown golden")
        .get_pages()
        .len();
    let current_pages = Document::load_mem(&current)
        .expect("parse generated long Markdown")
        .get_pages()
        .len();
    eprintln!("markdown/long-pagination: golden={golden_pages} current={current_pages}");
    assert_eq!(
        current_pages, golden_pages,
        "generated page count must come from the frozen golden"
    );

    let golden_words: Vec<_> = extract_text_pages(&golden)
        .expect("extract golden pages")
        .iter()
        .map(|page| word_multiset(page, false))
        .collect();
    let current_words: Vec<_> = extract_text_pages(&current)
        .expect("extract generated pages")
        .iter()
        .map(|page| word_multiset(page, false))
        .collect();
    assert_eq!(
        current_words, golden_words,
        "each generated page must contain the golden page's word multiset"
    );
}

#[test]
#[serial(markdown_pagination)]
fn remaining_markdown_and_csv_rows_match_golden_pagination_and_page_words() {
    let root = root();
    register_packaged_fonts(&root);
    for (name, input_path, golden_path) in [
        (
            "markdown/file1",
            "tests/fixtures/samples/md/file1.md",
            "tests/fixtures/corpus/legacy-goldens/markdown/file1.chromium.pdf",
        ),
        (
            "markdown/file2",
            "tests/fixtures/samples/md/file2.md",
            "tests/fixtures/corpus/legacy-goldens/markdown/file2.chromium.pdf",
        ),
        (
            "markdown/unicode-mixed",
            "tests/fixtures/corpus/legacy-goldens/inputs/unicode-mixed.md",
            "tests/fixtures/corpus/legacy-goldens/markdown/unicode-mixed.chromium.pdf",
        ),
        (
            "markdown/gfm-constructs",
            "tests/fixtures/corpus/legacy-goldens/inputs/gfm-constructs.md",
            "tests/fixtures/corpus/legacy-goldens/markdown/gfm-constructs.chromium.pdf",
        ),
    ] {
        let input = std::fs::read_to_string(root.join(input_path))
            .unwrap_or_else(|error| panic!("read {name} input: {error}"));
        let current = markdown_to_pdf(&input, &DocOptions::default())
            .unwrap_or_else(|error| panic!("render {name}: {error}"));
        let golden = std::fs::read(root.join(golden_path))
            .unwrap_or_else(|error| panic!("read {name} golden: {error}"));
        assert_golden_pages_and_words(name, &current, &golden);

        if name == "markdown/unicode-mixed" {
            let logical = extract_text_pages(&current)
                .expect("extract generated Unicode Markdown")
                .join("\n");
            for probe in [
                "これは日本語のテストです。漢字、ひらがな、カタカナを含みます。句読点、そして「かぎ括弧」も。",
                "这是一段简体中文测试文字。包含标点符号，以及数字一二三。",
                "한국어 테스트 문장입니다. 음절 블록과 자모를 포함합니다.",
                "هذا نص تجريبي باللغة العربية. يحتوي على أرقام عربية ٠١٢٣ وحروف متصلة.",
                "זהו טקסט בדיקה בעברית. כולל ניקוד: שָׁלוֹם.",
                "यह देवनागरी में एक परीक्षण वाक्य है। संयुक्ताक्षर: क्ष त्र ज्ञ।",
                "นี่คือข้อความทดสอบภาษาไทยที่ไม่มีการเว้นวรรคระหว่างคำและมีวรรณยุกต์ซ้อนกัน",
                "Это тестовый текст на русском языке. Включает букву Ё и знаки препинания.",
                "Αυτό είναι ένα δοκιμαστικό κείμενο στα ελληνικά.",
                "café Zürich René naïve coöp Ærø façade jalapeño",
                "Thumbs up 👍🏽 and a family 👨‍👩‍👧‍👦 plus a heart ❤️.",
                "Total: 123 ريال",
                "End of corpus.",
            ] {
                assert!(
                    logical.contains(probe),
                    "{name}: logical-order probe missing: {probe:?}"
                );
            }
        }
    }

    let csv_options = DocOptions {
        page_size: PageSize::LetterLandscape,
        ..DocOptions::default()
    };
    for (name, input_path, golden_path) in [
        (
            "csv/file1",
            "tests/fixtures/samples/csv/file1.csv",
            "tests/fixtures/corpus/legacy-goldens/csv/file1.chromium.pdf",
        ),
        (
            "csv/file2",
            "tests/fixtures/samples/csv/file2.csv",
            "tests/fixtures/corpus/legacy-goldens/csv/file2.chromium.pdf",
        ),
        (
            "csv/latin1",
            "tests/fixtures/corpus/legacy-goldens/inputs/latin1.csv",
            "tests/fixtures/corpus/legacy-goldens/csv/latin1.chromium.pdf",
        ),
        (
            "csv/unicode",
            "tests/fixtures/corpus/legacy-goldens/inputs/unicode.csv",
            "tests/fixtures/corpus/legacy-goldens/csv/unicode.chromium.pdf",
        ),
    ] {
        let input = std::fs::read(root.join(input_path))
            .unwrap_or_else(|error| panic!("read {name} input: {error}"));
        let current = csv_to_pdf(&input, &csv_options)
            .unwrap_or_else(|error| panic!("render {name}: {error}"));
        let golden = std::fs::read(root.join(golden_path))
            .unwrap_or_else(|error| panic!("read {name} golden: {error}"));
        assert_golden_pages_and_words(name, &current, &golden);
        if name == "csv/unicode" {
            let logical = extract_text_pages(&current)
                .expect("extract generated Unicode CSV")
                .join("\n");
            for probe in ["اختبار عربي", "בדיקה עברית", "Total: 123 ريال"] {
                assert!(
                    logical.contains(probe),
                    "{name}: logical-order probe missing: {probe:?}"
                );
            }
        }
    }
}
