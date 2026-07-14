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
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum total decompressed size across all parts (zip-bomb guard).
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// Small valid packages get this much decompression headroom.
const MIN_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
/// Larger packages may expand up to this multiple, capped by `MAX_TOTAL_BYTES`.
const MAX_EXPANSION_RATIO: u64 = 200;
/// Never reserve a whole part from untrusted ZIP size metadata up front.
const MAX_INITIAL_PART_CAPACITY: u64 = 64 * 1024;
/// Relationships fan out asset resolution and are attacker-controlled XML.
const MAX_RELATIONSHIPS_PER_PART: usize = 50_000;
/// Prevent pathological `../` or segment chains during target normalization.
const MAX_RELATIONSHIP_TARGET_DEPTH: usize = 64;

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

#[derive(Debug, Clone, Copy)]
struct PackageLimits {
    max_entries: usize,
    max_part_bytes: u64,
    max_total_bytes: u64,
}

impl PackageLimits {
    fn for_input(input_bytes: usize) -> PackageLimits {
        let input_bytes = u64::try_from(input_bytes).unwrap_or(u64::MAX);
        let proportional = input_bytes.saturating_mul(MAX_EXPANSION_RATIO);
        PackageLimits {
            max_entries: MAX_ENTRIES,
            max_part_bytes: MAX_PART_BYTES,
            max_total_bytes: proportional.clamp(MIN_TOTAL_BYTES, MAX_TOTAL_BYTES),
        }
    }
}

fn read_part_bounded<R: Read>(
    reader: &mut R,
    name: &str,
    declared_size: u64,
    max_part_bytes: u64,
    remaining_total_bytes: u64,
) -> Result<Vec<u8>> {
    if declared_size > max_part_bytes {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!("part `{name}` is {declared_size} bytes (part size limit {max_part_bytes})"),
        ));
    }
    if declared_size > remaining_total_bytes {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!("part `{name}` would exceed the package total size limit"),
        ));
    }

    let read_limit = max_part_bytes.min(remaining_total_bytes);
    let initial_capacity = declared_size.min(read_limit).min(MAX_INITIAL_PART_CAPACITY) as usize;
    let mut bytes = Vec::with_capacity(initial_capacity);
    reader
        .take(read_limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| OfficeError::Invalid(format!("failed reading `{name}`: {error}")))?;

    let actual_size = bytes.len() as u64;
    if actual_size > max_part_bytes {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!("part `{name}` exceeded the part size limit while decompressing"),
        ));
    }
    if actual_size > remaining_total_bytes {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!("part `{name}` exceeded the package total size limit while decompressing"),
        ));
    }
    Ok(bytes)
}

impl Package {
    /// Opens and validates an OOXML package from raw bytes.
    ///
    /// Fails with:
    /// - `Unsupported{Encrypted}` / `Unsupported{LegacyFormat}` for a CFB container
    ///   (encrypted OOXML vs legacy `.doc`/`.ppt`/`.xls`),
    /// - `Unsupported{DecompressionLimit}` for a tripped entry/part/total
    ///   decompression budget (an envelope miss routed to the fallback),
    /// - `Invalid` for non-zip input.
    pub fn open(bytes: &[u8]) -> Result<Package> {
        Self::open_with_limits(bytes, PackageLimits::for_input(bytes.len()))
    }

    fn open_with_limits(bytes: &[u8], limits: PackageLimits) -> Result<Package> {
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

        if archive.len() > limits.max_entries {
            return Err(OfficeError::unsupported(
                UnsupportedCode::DecompressionLimit,
                format!(
                    "package has {} entries (limit {})",
                    archive.len(),
                    limits.max_entries,
                ),
            ));
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
            let name = normalize_part(file.name());
            let declared_size = file.size();
            let remaining = limits.max_total_bytes.saturating_sub(total);
            let buf = read_part_bounded(
                &mut file,
                &name,
                declared_size,
                limits.max_part_bytes,
                remaining,
            )?;
            total = total.saturating_add(buf.len() as u64);
            parts.insert(name, buf);
        }

        let (ct_defaults, ct_overrides) = parse_content_types(&parts)?;
        // Validate every XML-bearing part once before format-specific code can
        // silently skip or deeply walk an auxiliary part.
        for (name, bytes) in &parts {
            if name.ends_with(".xml") || name.ends_with(".rels") {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| OfficeError::Invalid(format!("`{name}` is not UTF-8")))?;
                oxml::guard_xml_budget(text, name)?;
            }
        }
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
        let doc = oxml::parse_xml(text, &rels_name)?;

        let base_dir = part_dir(&normalize_part(source_part));
        let mut out = Vec::new();
        for rel in oxml::children(doc.root_element()) {
            if !oxml::is(rel, PKG_REL, "Relationship") {
                continue;
            }
            if out.len() >= MAX_RELATIONSHIPS_PER_PART {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::DecompressionLimit,
                    format!(
                        "`{rels_name}` relationship count exceeds limit {MAX_RELATIONSHIPS_PER_PART}"
                    ),
                ));
            }
            let id = oxml::attr_local(rel, "Id").unwrap_or_default().to_string();
            let rel_type = oxml::attr_local(rel, "Type")
                .unwrap_or_default()
                .to_string();
            let raw_target = oxml::attr_local(rel, "Target").unwrap_or_default();
            if raw_target.split('/').count() > MAX_RELATIONSHIP_TARGET_DEPTH {
                return Err(OfficeError::unsupported(
                    UnsupportedCode::DecompressionLimit,
                    format!(
                        "`{rels_name}` relationship target depth exceeds limit {MAX_RELATIONSHIP_TARGET_DEPTH}"
                    ),
                ));
            }
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
    let doc = oxml::parse_xml(text, "[Content_Types].xml")?;

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
    use std::io::Write;

    #[test]
    fn bounded_part_reader_uses_actual_bytes_not_declared_size() {
        let mut reader = Cursor::new(vec![0_u8; 17]);
        let err = read_part_bounded(&mut reader, "word/document.xml", 1, 16, 100)
            .expect_err("actual bytes must trip the per-part budget");
        assert!(matches!(
            err,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                detail,
            } if detail.contains("size limit")
        ));
    }

    #[test]
    fn bounded_part_reader_enforces_actual_aggregate_remaining_budget() {
        let mut reader = Cursor::new(vec![0_u8; 9]);
        let err = read_part_bounded(&mut reader, "word/document.xml", 1, 100, 8)
            .expect_err("actual bytes must trip the aggregate budget");
        assert!(matches!(
            err,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                detail,
            } if detail.contains("total size limit")
        ));
    }

    /// A budget hit must present as an envelope miss (`unsupported`), never as
    /// corruption — DocKosha routes it to the compatibility service.
    #[test]
    fn decompression_budget_is_an_envelope_miss_not_corruption() {
        let err = read_part_bounded(
            &mut Cursor::new(Vec::new()),
            "word/document.xml",
            u64::MAX,
            16,
            100,
        )
        .expect_err("declared size over the part budget must be rejected");
        assert!(err.to_string().starts_with("unsupported"));
    }

    #[test]
    fn hundred_thousand_zip_entries_fail_with_typed_limit() {
        let started = std::time::Instant::now();
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for index in 0..100_000 {
                zip.start_file(format!("p{index}"), options).unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap();
        }
        let error = Package::open(cursor.get_ref()).expect_err("entry limit");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                ..
            }
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

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
