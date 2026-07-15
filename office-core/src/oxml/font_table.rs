//! Word `fontTable.xml` metadata used by text-free font preflight.
//!
//! Embedded ODTTF decoding remains owned by `layout::assemble`; this parser
//! reads only identity/provider hints and whether an authored style has an
//! embedded payload.

use super::{W, attr, child, children, is, parse_xml};
use crate::Result;

/// Parsed Word font table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontTable {
    /// Declared font entries in document order.
    pub fonts: Vec<FontTableEntry>,
}

/// Metadata for one `<w:font>` declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontTableEntry {
    /// Authored family name.
    pub family: String,
    /// Alternate family name.
    pub alt_name: Option<String>,
    /// Ten-byte PANOSE as uppercase hexadecimal.
    pub panose: Option<String>,
    /// OOXML family classification.
    pub family_class: Option<String>,
    /// OOXML pitch classification.
    pub pitch: Option<String>,
    /// OOXML charset byte as hexadecimal.
    pub charset: Option<String>,
    /// OS/2 Unicode range bitfields (`usb0` through `usb3`).
    pub unicode_ranges: Vec<u32>,
    /// OS/2 codepage range bitfields (`csb0` and `csb1`).
    pub codepage_ranges: Vec<u32>,
    embedded_regular: bool,
    embedded_bold: bool,
    embedded_italic: bool,
    embedded_bold_italic: bool,
}

impl FontTable {
    /// Parses a complete `word/fontTable.xml` part.
    pub fn parse(xml: &str) -> Result<FontTable> {
        let document = parse_xml(xml, "fontTable")?;
        let mut table = FontTable::default();
        for node in children(document.root_element()) {
            if !is(node, W, "font") {
                continue;
            }
            let Some(family) = attr(node, W, "name") else {
                continue;
            };
            let signature = child(node, W, "sig");
            table.fonts.push(FontTableEntry {
                family: family.to_string(),
                alt_name: value_child(node, "altName"),
                panose: value_child(node, "panose1").map(|value| value.to_ascii_uppercase()),
                family_class: value_child(node, "family"),
                pitch: value_child(node, "pitch"),
                charset: value_child(node, "charset").map(|value| value.to_ascii_uppercase()),
                unicode_ranges: signature
                    .into_iter()
                    .flat_map(|sig| ["usb0", "usb1", "usb2", "usb3"].map(|key| hex_attr(sig, key)))
                    .flatten()
                    .collect(),
                codepage_ranges: signature
                    .into_iter()
                    .flat_map(|sig| ["csb0", "csb1"].map(|key| hex_attr(sig, key)))
                    .flatten()
                    .collect(),
                embedded_regular: child(node, W, "embedRegular").is_some(),
                embedded_bold: child(node, W, "embedBold").is_some(),
                embedded_italic: child(node, W, "embedItalic").is_some(),
                embedded_bold_italic: child(node, W, "embedBoldItalic").is_some(),
            });
        }
        Ok(table)
    }

    /// Finds metadata by case-insensitive family or alternate-family name.
    pub fn find(&self, family: &str) -> Option<&FontTableEntry> {
        self.fonts.iter().find(|entry| {
            entry.family.eq_ignore_ascii_case(family)
                || entry
                    .alt_name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(family))
        })
    }
}

impl FontTableEntry {
    /// Whether the exact bold/italic style is supplied inside the DOCX.
    pub fn has_embedded_style(&self, weight: u16, italic: bool) -> bool {
        match (weight >= 600, italic) {
            (false, false) => self.embedded_regular,
            (true, false) => self.embedded_bold,
            (false, true) => self.embedded_italic,
            (true, true) => self.embedded_bold_italic,
        }
    }
}

fn value_child(node: roxmltree::Node<'_, '_>, local: &str) -> Option<String> {
    child(node, W, local)
        .and_then(|value| attr(value, W, "val"))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn hex_attr(node: roxmltree::Node<'_, '_>, local: &str) -> Option<u32> {
    attr(node, W, local).and_then(|value| u32::from_str_radix(value, 16).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_metadata_and_embedded_style_flags() {
        let xml = format!(
            r#"<w:fonts xmlns:w="{W}"><w:font w:name="Example"><w:altName w:val="Alias"/><w:panose1 w:val="020b"/><w:sig w:usb0="00000001"/><w:embedBold/></w:font></w:fonts>"#
        );
        let table = FontTable::parse(&xml).unwrap();
        let entry = table.find("alias").unwrap();
        assert_eq!(entry.panose.as_deref(), Some("020B"));
        assert_eq!(entry.unicode_ranges, [1]);
        assert!(entry.has_embedded_style(700, false));
        assert!(!entry.has_embedded_style(400, false));
    }
}
