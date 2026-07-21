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

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const EOCD_MIN_LEN: usize = 22;
const ZIP64_LOCATOR_LEN: usize = 20;

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

fn checked_slice<const N: usize>(bytes: &[u8], offset: usize, label: &str) -> Result<[u8; N]> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| OfficeError::Invalid(format!("{label} offset overflow")))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| OfficeError::Invalid(format!("truncated {label}")))?
        .try_into()
        .map_err(|_| OfficeError::Invalid(format!("truncated {label}")))
}

fn field_offset(base: usize, field: usize, label: &str) -> Result<usize> {
    base.checked_add(field)
        .ok_or_else(|| OfficeError::Invalid(format!("{label} field offset overflows")))
}

fn read_u16(bytes: &[u8], base: usize, field: usize, label: &str) -> Result<u16> {
    checked_slice(bytes, field_offset(base, field, label)?, label).map(u16::from_le_bytes)
}

fn read_u32(bytes: &[u8], base: usize, field: usize, label: &str) -> Result<u32> {
    checked_slice(bytes, field_offset(base, field, label)?, label).map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], base: usize, field: usize, label: &str) -> Result<u64> {
    checked_slice(bytes, field_offset(base, field, label)?, label).map(u64::from_le_bytes)
}

fn find_eocd(bytes: &[u8]) -> Result<usize> {
    let last = bytes
        .len()
        .checked_sub(EOCD_MIN_LEN)
        .ok_or_else(|| OfficeError::Invalid("zip is too short for an EOCD record".into()))?;
    let first = bytes
        .len()
        .saturating_sub(EOCD_MIN_LEN + usize::from(u16::MAX));
    for offset in (first..=last).rev() {
        if read_u32(bytes, offset, 0, "EOCD signature")? != EOCD_SIGNATURE {
            continue;
        }
        let comment_len = usize::from(read_u16(bytes, offset, 20, "EOCD comment length")?);
        let record_end = offset
            .checked_add(EOCD_MIN_LEN)
            .and_then(|end| end.checked_add(comment_len));
        if record_end == Some(bytes.len()) {
            return Ok(offset);
        }
    }
    Err(OfficeError::Invalid(
        "zip has no consistent end-of-central-directory record".into(),
    ))
}

fn reject_entry_count(entries: u64, limit: usize) -> Result<usize> {
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if entries > limit_u64 {
        return Err(OfficeError::unsupported(
            UnsupportedCode::DecompressionLimit,
            format!("package declares {entries} entries (limit {limit})"),
        ));
    }
    usize::try_from(entries)
        .map_err(|_| OfficeError::Invalid("ZIP entry count does not fit this platform".into()))
}

fn validate_central_directory_bounds(offset: u64, size: u64, before: usize) -> Result<()> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| OfficeError::Invalid("ZIP central-directory range overflows".into()))?;
    if end > u64::try_from(before).unwrap_or(u64::MAX) {
        return Err(OfficeError::Invalid(
            "ZIP central directory overlaps its end metadata".into(),
        ));
    }
    Ok(())
}

fn preflight_zip64_entry_count(
    bytes: &[u8],
    eocd_offset: usize,
    classic_entries_on_disk: u16,
    classic_entries_total: u16,
    classic_directory_size: u32,
    classic_directory_offset: u32,
    limit: usize,
) -> Result<usize> {
    let locator_offset = eocd_offset
        .checked_sub(ZIP64_LOCATOR_LEN)
        .ok_or_else(|| OfficeError::Invalid("ZIP64 EOCD is missing its locator record".into()))?;
    if read_u32(bytes, locator_offset, 0, "ZIP64 locator signature")? != ZIP64_LOCATOR_SIGNATURE {
        return Err(OfficeError::Invalid(
            "ZIP64 EOCD is missing its locator record".into(),
        ));
    }
    let locator_disk = read_u32(bytes, locator_offset, 4, "ZIP64 locator disk")?;
    let record_offset_u64 = read_u64(bytes, locator_offset, 8, "ZIP64 EOCD offset")?;
    let disk_count = read_u32(bytes, locator_offset, 16, "ZIP64 disk count")?;
    if locator_disk != 0 || disk_count != 1 {
        return Err(OfficeError::Invalid(
            "multi-disk ZIP64 packages are not supported".into(),
        ));
    }
    let record_offset = usize::try_from(record_offset_u64)
        .map_err(|_| OfficeError::Invalid("ZIP64 EOCD offset does not fit this platform".into()))?;
    if read_u32(bytes, record_offset, 0, "ZIP64 EOCD signature")? != ZIP64_EOCD_SIGNATURE {
        return Err(OfficeError::Invalid(
            "ZIP64 locator does not reference a ZIP64 EOCD record".into(),
        ));
    }
    let record_body_size = read_u64(bytes, record_offset, 4, "ZIP64 EOCD size")?;
    if record_body_size < 44 {
        return Err(OfficeError::Invalid(
            "ZIP64 EOCD record is shorter than its fixed fields".into(),
        ));
    }
    let record_end_u64 = record_offset_u64
        .checked_add(12)
        .and_then(|offset| offset.checked_add(record_body_size))
        .ok_or_else(|| OfficeError::Invalid("ZIP64 EOCD record range overflows".into()))?;
    if record_end_u64 != u64::try_from(locator_offset).unwrap_or(u64::MAX) {
        return Err(OfficeError::Invalid(
            "ZIP64 EOCD size is inconsistent with its locator".into(),
        ));
    }
    let disk = read_u32(bytes, record_offset, 16, "ZIP64 disk number")?;
    let central_disk = read_u32(bytes, record_offset, 20, "ZIP64 central-directory disk")?;
    let entries_on_disk = read_u64(bytes, record_offset, 24, "ZIP64 entries on disk")?;
    let entries_total = read_u64(bytes, record_offset, 32, "ZIP64 total entries")?;
    if disk != 0 || central_disk != 0 {
        return Err(OfficeError::Invalid(
            "multi-disk ZIP64 packages are not supported".into(),
        ));
    }
    if entries_on_disk != entries_total {
        return Err(OfficeError::Invalid(
            "ZIP64 per-disk and total entry counts disagree".into(),
        ));
    }
    let classic_count_matches =
        |classic: u16| classic == u16::MAX || u64::from(classic) == entries_total;
    if !classic_count_matches(classic_entries_on_disk)
        || !classic_count_matches(classic_entries_total)
    {
        return Err(OfficeError::Invalid(
            "classic and ZIP64 entry counts disagree".into(),
        ));
    }
    let entries = reject_entry_count(entries_total, limit)?;
    let directory_size = read_u64(bytes, record_offset, 40, "ZIP64 directory size")?;
    let directory_offset = read_u64(bytes, record_offset, 48, "ZIP64 directory offset")?;
    if classic_directory_size != u32::MAX && u64::from(classic_directory_size) != directory_size
        || classic_directory_offset != u32::MAX
            && u64::from(classic_directory_offset) != directory_offset
    {
        return Err(OfficeError::Invalid(
            "classic and ZIP64 central-directory metadata disagree".into(),
        ));
    }
    validate_central_directory_bounds(directory_offset, directory_size, record_offset)?;
    Ok(entries)
}

fn preflight_entry_count(bytes: &[u8], limit: usize) -> Result<usize> {
    let eocd_offset = find_eocd(bytes)?;
    let disk = read_u16(bytes, eocd_offset, 4, "EOCD disk number")?;
    let central_disk = read_u16(bytes, eocd_offset, 6, "EOCD central-directory disk")?;
    if disk != 0 || central_disk != 0 {
        return Err(OfficeError::Invalid(
            "multi-disk ZIP packages are not supported".into(),
        ));
    }
    let entries_on_disk = read_u16(bytes, eocd_offset, 8, "EOCD entries on disk")?;
    let entries_total = read_u16(bytes, eocd_offset, 10, "EOCD total entries")?;
    let directory_size = read_u32(bytes, eocd_offset, 12, "EOCD directory size")?;
    let directory_offset = read_u32(bytes, eocd_offset, 16, "EOCD directory offset")?;
    let is_zip64 = entries_on_disk == u16::MAX
        || entries_total == u16::MAX
        || directory_size == u32::MAX
        || directory_offset == u32::MAX;
    if is_zip64 {
        return preflight_zip64_entry_count(
            bytes,
            eocd_offset,
            entries_on_disk,
            entries_total,
            directory_size,
            directory_offset,
            limit,
        );
    }
    if entries_on_disk != entries_total {
        return Err(OfficeError::Invalid(
            "ZIP per-disk and total entry counts disagree".into(),
        ));
    }
    let entries = reject_entry_count(u64::from(entries_total), limit)?;
    validate_central_directory_bounds(
        u64::from(directory_offset),
        u64::from(directory_size),
        eocd_offset,
    )?;
    Ok(entries)
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

        preflight_entry_count(bytes, limits.max_entries)?;

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

    fn classic_eocd(
        disk: u16,
        central_disk: u16,
        entries_on_disk: u16,
        entries_total: u16,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&disk.to_le_bytes());
        bytes.extend_from_slice(&central_disk.to_le_bytes());
        bytes.extend_from_slice(&entries_on_disk.to_le_bytes());
        bytes.extend_from_slice(&entries_total.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes
    }

    fn zip64_eocd(entries: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&44_u64.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&entries.to_le_bytes());
        bytes.extend_from_slice(&entries.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&classic_eocd(0, 0, u16::MAX, u16::MAX));
        bytes
    }

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
    fn classic_declared_entry_count_is_preflighted_with_an_injectable_limit() {
        let bytes = classic_eocd(0, 0, 2, 2);
        let error = preflight_entry_count(&bytes, 1).expect_err("entry limit");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                ..
            }
        ));
    }

    #[test]
    fn zip64_declared_entry_count_is_preflighted_before_archive_open() {
        let bytes = zip64_eocd((MAX_ENTRIES + 1) as u64);
        let error = preflight_entry_count(&bytes, MAX_ENTRIES).expect_err("entry limit");
        assert!(matches!(
            error,
            OfficeError::Unsupported {
                code: UnsupportedCode::DecompressionLimit,
                ..
            }
        ));
    }

    #[test]
    fn multi_disk_and_inconsistent_counts_are_invalid() {
        for bytes in [classic_eocd(1, 0, 0, 0), classic_eocd(0, 0, 1, 2)] {
            assert!(matches!(
                preflight_entry_count(&bytes, MAX_ENTRIES),
                Err(OfficeError::Invalid(_))
            ));
        }
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
