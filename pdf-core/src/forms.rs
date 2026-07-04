//! Zone 2 — AcroForm support via pdfium: enumerate fields, fill text fields,
//! and flatten forms/annotations into page content.

use pdfium_render::prelude::*;

use crate::error::{PdfCoreError, Result};

/// Form field kinds we distinguish; everything else maps to `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Checkbox,
    RadioButton,
    ComboBox,
    ListBox,
    Signature,
    Other,
}

/// A form field with its current value (if any).
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub name: String,
    pub kind: FieldKind,
    pub value: Option<String>,
}

fn kind_of(field: &PdfFormField) -> FieldKind {
    match field.field_type() {
        PdfFormFieldType::Text => FieldKind::Text,
        PdfFormFieldType::Checkbox => FieldKind::Checkbox,
        PdfFormFieldType::RadioButton => FieldKind::RadioButton,
        PdfFormFieldType::ComboBox => FieldKind::ComboBox,
        PdfFormFieldType::ListBox => FieldKind::ListBox,
        PdfFormFieldType::Signature => FieldKind::Signature,
        _ => FieldKind::Other,
    }
}

/// Extracts a string representation of the field's current value, mirroring
/// pdfium-render's own `PdfForm::field_values()` conventions: checkboxes and
/// radio buttons report `"true"`/`"false"` (radios report their group export
/// value when checked); buttons, signatures, and unknown fields report `None`.
fn value_of(field: &PdfFormField) -> Option<String> {
    match field.field_type() {
        PdfFormFieldType::Text => field.as_text_field().and_then(|f| f.value()),
        PdfFormFieldType::ComboBox => field.as_combo_box_field().and_then(|f| f.value()),
        PdfFormFieldType::ListBox => field.as_list_box_field().and_then(|f| f.value()),
        PdfFormFieldType::Checkbox => field.as_checkbox_field().map(|f| {
            if f.is_checked().unwrap_or(false) {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }),
        PdfFormFieldType::RadioButton => field.as_radio_button_field().and_then(|f| {
            if f.is_checked().unwrap_or(false) {
                f.group_value()
            } else {
                Some("false".to_string())
            }
        }),
        _ => None,
    }
}

/// Lists all form fields in the document (empty vec if there is no AcroForm).
pub fn list_fields(pdf: &[u8]) -> Result<Vec<FormField>> {
    let pdfium = crate::pdfium::lock()?;
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;

    // Without a form-fill environment (i.e. no AcroForm), widget annotations
    // never expose form fields — nothing to enumerate.
    if document.form().is_none() {
        return Ok(Vec::new());
    }

    let mut fields = Vec::new();
    for page in document.pages().iter() {
        for annotation in page.annotations().iter() {
            if let Some(field) = annotation.as_form_field() {
                fields.push(FormField {
                    name: field.name().unwrap_or_default(),
                    kind: kind_of(field),
                    value: value_of(field),
                });
            }
        }
    }
    Ok(fields)
}

/// Sets text-field values by field name. Unknown names error with
/// `PdfCoreError::InvalidInput`; non-text fields error with `Unsupported`.
pub fn fill_fields(pdf: &[u8], values: &[(String, String)]) -> Result<Vec<u8>> {
    let pdfium = crate::pdfium::lock()?;
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;

    if document.form().is_none() && !values.is_empty() {
        let (name, _) = &values[0];
        return Err(PdfCoreError::InvalidInput(format!(
            "no form field named '{name}': document has no AcroForm"
        )));
    }

    for (name, value) in values {
        set_text_field(&document, name, value)?;
    }

    Ok(document.save_to_bytes()?)
}

/// Locates the named field across all pages and sets its value; the field
/// must be a text field.
fn set_text_field(document: &PdfDocument, name: &str, value: &str) -> Result<()> {
    for page in document.pages().iter() {
        let annotations = page.annotations();
        for index in annotations.as_range() {
            let Ok(mut annotation) = annotations.get(index) else {
                continue;
            };
            let Some(field) = annotation.as_form_field_mut() else {
                continue;
            };
            if field.name().as_deref() != Some(name) {
                continue;
            }
            let kind = kind_of(field);
            let Some(text_field) = field.as_text_field_mut() else {
                return Err(PdfCoreError::Unsupported(format!(
                    "field '{name}' has kind {kind:?}; only text fields can be filled"
                )));
            };
            text_field.set_value(value)?;
            return Ok(());
        }
    }
    Err(PdfCoreError::InvalidInput(format!(
        "no form field named '{name}' in document"
    )))
}

/// Flattens form fields and annotations into static page content.
pub fn flatten(pdf: &[u8]) -> Result<Vec<u8>> {
    let pdfium = crate::pdfium::lock()?;
    let document = pdfium.load_pdf_from_byte_slice(pdf, None)?;

    for mut page in document.pages().iter() {
        page.flatten()?;
    }

    Ok(document.save_to_bytes()?)
}
