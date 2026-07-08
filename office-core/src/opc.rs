//! The OPC (Open Packaging Conventions) container: OOXML files are ZIP archives
//! ("packages") of XML "parts" plus `[Content_Types].xml` and `_rels` metadata.
//!
//! [`Package::open`] validates the container (with zip-bomb guards) and detects
//! encrypted / legacy-binary containers up front so the engine can fail closed.

use crate::oxml::{self, PKG_REL};
use crate::{OfficeError, Result, UnsupportedCode};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

/// Maximum number of parts in a package (zip-bomb guard).
const MAX_ENTRIES: usize = 10_000;
/// Maximum decompressed size of any single part (zip-bomb guard).
const MAX_PART_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum total decompressed size across all parts (zip-bomb guard).
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// OLE2 / CFB compound-file magic (`D0 CF 11 E0 A1 B1 1A E1`). Legacy binary
/// Office formats and encrypted OOXML both use this container.
const CFB_MAGIC: [u8; 4] = [0xD0, 0xCF, 0x11, 0xE0];

/// The relationship type of a package's main document part.
pub const REL_OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";

/// Whether a relationship target is inside the package or an external URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// Target resolves to a part within this package.
    Internal,
    /// Target is an external URI (e.g. a hyperlink); kept verbatim.
    External,
}

/// A parsed OPC relationship.
#[derive(Debug, Clone)]
pub struct Relationship {
    /// The relationship id (`rId7`).
    pub id: String,
    /// The relationship type URI.
    pub rel_type: String,
    /// For [`TargetMode::Internal`], the resolved part name (lowercased, no
    /// leading `/`); for [`TargetMode::External`], the raw URI.
    pub target: String,
    /// Internal vs external.
    pub mode: TargetMode,
}

/// An opened OPC package: a map of normalized part name → decompressed bytes,
/// plus the parsed `[Content_Types].xml`.
#[derive(Debug)]
pub struct Package {
    parts: BTreeMap<String, Vec<u8>>,
    ct_defaults: BTreeMap<String, String>,
    ct_overrides: BTreeMap<String, String>,
}

impl Package {
    /// Opens and validates an OOXML package from raw bytes.
    ///
    /// Fails with:
    /// - `Unsupported{Encrypted}` / `Unsupported{LegacyFormat}` for a CFB container
    ///   (encrypted OOXML vs legacy `.doc`/`.ppt`/`.xls`),
    /// - `Invalid` for non-zip input or a tripped zip-bomb guard.
    pub fn open(bytes: &[u8]) -> Result<Package> {
        if bytes.len() >= 4 && bytes[0..4] == CFB_MAGIC {
            return Err(if looks_encrypted(bytes) {
                OfficeError::unsupported(UnsupportedCode::Encrypted, "encrypted OOXML package")
            } else {
                OfficeError::unsupported(
                    UnsupportedCode::LegacyFormat,
                    "legacy CFB (OLE2) container",
                )
            });
        }

        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| OfficeError::Invalid(format!("not a valid zip/OPC package: {e}")))?;

        if archive.len() > MAX_ENTRIES {
            return Err(OfficeError::Invalid(format!(
                "package has {} entries (limit {MAX_ENTRIES})",
                archive.len()
            )));
        }

        let mut parts = BTreeMap::new();
        let mut total: u64 = 0;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| OfficeError::Invalid(format!("corrupt zip entry {i}: {e}")))?;
            if !file.is_file() {
                continue;
            }
            let size = file.size();
            if size > MAX_PART_BYTES {
                return Err(OfficeError::Invalid(format!(
                    "part `{}` is {size} bytes (limit {MAX_PART_BYTES})",
                    file.name()
                )));
            }
            total = total.saturating_add(size);
            if total > MAX_TOTAL_BYTES {
                return Err(OfficeError::Invalid(format!(
                    "package decompresses to over {MAX_TOTAL_BYTES} bytes"
                )));
            }
            let name = normalize_part(file.name());
            let mut buf = Vec::with_capacity(size as usize);
            file.read_to_end(&mut buf)
                .map_err(|e| OfficeError::Invalid(format!("failed reading `{name}`: {e}")))?;
            // Belt-and-suspenders: the declared size can lie; cap the actual read.
            if buf.len() as u64 > MAX_PART_BYTES {
                return Err(OfficeError::Invalid(format!(
                    "part `{name}` exceeded the size limit while decompressing"
                )));
            }
            parts.insert(name, buf);
        }

        let (ct_defaults, ct_overrides) = parse_content_types(&parts)?;
        Ok(Package {
            parts,
            ct_defaults,
            ct_overrides,
        })
    }

    /// Returns the bytes of a part by name (case-insensitive; leading `/` optional).
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.parts.get(&normalize_part(name)).map(Vec::as_slice)
    }

    /// Iterates all normalized part names in the package (deterministic order).
    pub fn part_names(&self) -> impl Iterator<Item = &str> {
        self.parts.keys().map(String::as_str)
    }

    /// The content type registered for `part`: an explicit `Override` wins,
    /// otherwise the `Default` for the part's extension.
    pub fn content_type(&self, part: &str) -> Option<&str> {
        let norm = normalize_part(part);
        if let Some(ct) = self.ct_overrides.get(&norm) {
            return Some(ct.as_str());
        }
        let ext = norm.rsplit('.').next()?.to_ascii_lowercase();
        self.ct_defaults.get(&ext).map(String::as_str)
    }

    /// Parses the relationships declared for `source_part` (`""` = package root).
    ///
    /// Internal targets are resolved against the source part's directory (with
    /// `..` normalization); external targets keep their raw URI.
    pub fn relationships(&self, source_part: &str) -> Result<Vec<Relationship>> {
        let rels_name = rels_part_name(source_part);
        let Some(bytes) = self.part(&rels_name) else {
            return Ok(Vec::new());
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|_| OfficeError::Invalid(format!("`{rels_name}` is not UTF-8")))?;
        let doc = roxmltree::Document::parse(text)
            .map_err(|e| OfficeError::Invalid(format!("`{rels_name}` XML: {e}")))?;

        let base_dir = part_dir(&normalize_part(source_part));
        let mut out = Vec::new();
        for rel in oxml::children(doc.root_element()) {
            if !oxml::is(rel, PKG_REL, "Relationship") {
                continue;
            }
            let id = oxml::attr_local(rel, "Id").unwrap_or_default().to_string();
            let rel_type = oxml::attr_local(rel, "Type")
                .unwrap_or_default()
                .to_string();
            let raw_target = oxml::attr_local(rel, "Target").unwrap_or_default();
            let external = oxml::attr_local(rel, "TargetMode") == Some("External");
            let (mode, target) = if external {
                (TargetMode::External, raw_target.to_string())
            } else {
                (TargetMode::Internal, resolve_target(&base_dir, raw_target))
            };
            out.push(Relationship {
                id,
                rel_type,
                target,
                mode,
            });
        }
        Ok(out)
    }

    /// The package's main document part name (the package-root relationship of
    /// type `officeDocument`).
    pub fn main_part(&self) -> Result<String> {
        self.relationships("")?
            .into_iter()
            .find(|r| r.rel_type == REL_OFFICE_DOCUMENT)
            .map(|r| r.target)
            .ok_or_else(|| {
                OfficeError::Invalid("package has no officeDocument relationship".into())
            })
    }
}

/// Normalizes a part name to lowercase, `/`-separated, without a leading `/`.
fn normalize_part(name: &str) -> String {
    name.trim_start_matches('/')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// The directory of a normalized part name (`word/document.xml` → `word`).
fn part_dir(part: &str) -> String {
    match part.rfind('/') {
        Some(i) => part[..i].to_string(),
        None => String::new(),
    }
}

/// The `_rels` part name for a source part (`""` → `_rels/.rels`).
fn rels_part_name(source_part: &str) -> String {
    let norm = normalize_part(source_part);
    if norm.is_empty() {
        return "_rels/.rels".to_string();
    }
    match norm.rfind('/') {
        Some(i) => format!("{}/_rels/{}.rels", &norm[..i], &norm[i + 1..]),
        None => format!("_rels/{norm}.rels"),
    }
}

/// Resolves a relationship target relative to `base_dir`, normalizing `.`/`..`.
fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(rest) = target.strip_prefix('/') {
        return normalize_part(rest);
    }
    let mut segments: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    segments.join("/").to_ascii_lowercase()
}

/// Parses `[Content_Types].xml` into (defaults-by-extension, overrides-by-part).
fn parse_content_types(
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let Some(bytes) = parts.get("[content_types].xml") else {
        return Err(OfficeError::Invalid(
            "package missing [Content_Types].xml".into(),
        ));
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| OfficeError::Invalid("[Content_Types].xml is not UTF-8".into()))?;
    let doc = roxmltree::Document::parse(text)
        .map_err(|e| OfficeError::Invalid(format!("[Content_Types].xml XML: {e}")))?;

    let mut defaults = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    for node in oxml::children(doc.root_element()) {
        let local = node.tag_name().name();
        let ct = oxml::attr_local(node, "ContentType");
        if local == "Default"
            && let (Some(ext), Some(ct)) = (oxml::attr_local(node, "Extension"), ct)
        {
            defaults.insert(ext.to_ascii_lowercase(), ct.to_string());
        } else if local == "Override"
            && let (Some(part), Some(ct)) = (oxml::attr_local(node, "PartName"), ct)
        {
            overrides.insert(normalize_part(part), ct.to_string());
        }
    }
    Ok((defaults, overrides))
}

/// Heuristic: a CFB container that contains the UTF-16LE marker `EncryptedPackage`
/// (or `EncryptionInfo`) is encrypted OOXML rather than a legacy binary document.
fn looks_encrypted(bytes: &[u8]) -> bool {
    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
    contains(bytes, &utf16le("EncryptedPackage")) || contains(bytes, &utf16le("EncryptionInfo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_name_normalization() {
        assert_eq!(normalize_part("/word/Document.xml"), "word/document.xml");
        assert_eq!(normalize_part("Word\\Header1.xml"), "word/header1.xml");
    }

    #[test]
    fn rels_naming() {
        assert_eq!(rels_part_name(""), "_rels/.rels");
        assert_eq!(
            rels_part_name("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
        assert_eq!(rels_part_name("document.xml"), "_rels/document.xml.rels");
    }

    #[test]
    fn target_resolution() {
        assert_eq!(
            resolve_target("word", "media/image1.png"),
            "word/media/image1.png"
        );
        assert_eq!(
            resolve_target("word", "../customXml/item1.xml"),
            "customxml/item1.xml"
        );
        assert_eq!(
            resolve_target("word", "/word/styles.xml"),
            "word/styles.xml"
        );
        assert_eq!(resolve_target("", "docProps/core.xml"), "docprops/core.xml");
    }

    #[test]
    fn cfb_detection() {
        let mut legacy = CFB_MAGIC.to_vec();
        legacy.extend_from_slice(&[0u8; 32]);
        let err = Package::open(&legacy).unwrap_err();
        assert!(matches!(
            err,
            OfficeError::Unsupported {
                code: UnsupportedCode::LegacyFormat,
                ..
            }
        ));
    }

    #[test]
    fn non_zip_is_invalid() {
        let err = Package::open(b"this is not a zip file at all").unwrap_err();
        assert!(matches!(err, OfficeError::Invalid(_)));
    }
}
