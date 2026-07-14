use office_core::testkit::gdocs_paragraph_docx;
use office_core::{ConvertOptions, OfficeFormat, convert};
use pdf_core::extract::extract_text;

#[test]
fn google_docs_conformance_battery_converts_and_extracts_body_probes() {
    let output = convert(
        &gdocs_paragraph_docx(),
        OfficeFormat::Docx,
        &ConvertOptions::default(),
    )
    .expect("convert Google Docs conformance battery");
    assert!(output.page_count >= 1);
    let text = extract_text(&output.pdf).expect("extract converted battery text");
    for probe in [
        "GDocs paragraph probe",
        "GDocs toggles probe",
        "GDocs table probe",
    ] {
        assert!(text.contains(probe), "missing {probe:?} in {text:?}");
    }
}
