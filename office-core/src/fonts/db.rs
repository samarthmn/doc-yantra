//! The font database: a set of parsed faces sourced from bundled OFL fonts,
//! (native) system directories, runtime registration, and the document's own
//! embedded fonts.

use crate::{OfficeError, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Bounds matching scans and per-face parser state, including system faces.
const MAX_FONT_FACES: usize = 1_024;

/// A stable handle to a face within a [`FontDb`] (index into its face list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub usize);

/// A single font face (one style of one family).
#[derive(Debug, Clone)]
pub struct Face {
    /// Primary family name (name id 1 / typographic family, or the embed hint).
    pub family_name: String,
    /// The full name (name id 4), when available (used for exact-name matching).
    pub full_name: Option<String>,
    /// Subfamily / style name (name id 2), e.g. "Bold Italic".
    pub subfamily: String,
    /// OS/2 usWeightClass (100–900; 400 regular, 700 bold).
    pub weight: u16,
    /// OS/2 width class (1–9; 5 is normal).
    pub stretch: u16,
    /// Whether the face is italic/oblique.
    pub italic: bool,
    /// Whether this face came from the document's embedded fonts (top priority).
    pub embedded: bool,
    /// SHA-256 of the complete source artifact.
    pub sha256: String,
    /// Whether the host pinned this artifact digest.
    pub hash_pinned: bool,
    /// Decoded OS/2 policy governing PDF embedding and subsetting.
    pub embedding_permission: super::registration::EmbeddingPermission,
    /// The raw sfnt bytes (shared; a TTC is shared across its faces).
    pub data: Arc<Vec<u8>>,
    /// Face index within a TrueType Collection (0 for a single font).
    pub index: u32,
}

impl Face {
    /// Parses this face with `ttf-parser` for metric/glyph queries.
    pub fn parsed(&self) -> Option<ttf_parser::Face<'_>> {
        ttf_parser::Face::parse(&self.data, self.index).ok()
    }

    /// Metrics for layout line boxes. Pan-CJK substitutes present the
    /// legacy CJK line convention so substituted documents keep Word's
    /// pagination (see [`super::matching::legacy_cjk_line_metrics_em`]).
    pub fn line_metrics(&self) -> Option<super::FaceMetrics<'_>> {
        let mut metrics = self.parsed().map(super::FaceMetrics::new)?;
        if let Some(class) = super::matching::legacy_cjk_class(&self.family_name) {
            metrics = metrics.with_legacy_class(class);
        }
        Some(if super::matching::pan_cjk_face(&self.family_name) {
            let (ascent, descent, line_gap) =
                super::matching::legacy_cjk_line_metrics_em(&self.family_name);
            metrics.with_line_metrics_em(ascent, descent, line_gap)
        } else if super::matching::legacy_cjk_centered_family(&self.family_name) {
            // Word sizes classic East Asian faces by its fixed 1.3em FE line
            // factor (growth centered on the 1.0em win body), not by the
            // font's own hhea gap — Batang carries a ~0.42em gap that Word
            // visibly does not honor. Same convention as the substitutes.
            let (ascent, descent, line_gap) =
                super::matching::legacy_cjk_line_metrics_em(&self.family_name);
            metrics.with_line_metrics_em(ascent, descent, line_gap)
        } else {
            metrics
        })
    }
}

/// A collection of faces with resolution (see [`super::matching`]).
#[derive(Debug, Clone, Default)]
pub struct FontDb {
    faces: Vec<Face>,
    resolution_mode: crate::ResolutionMode,
    classing_hints: BTreeMap<String, super::requests::FontClassing>,
}

impl FontDb {
    /// An empty database.
    pub fn new() -> FontDb {
        FontDb::default()
    }

    /// A database seeded with the bundled OFL fonts (`bundled-fonts` feature).
    ///
    /// Tolerant of an empty `fonts/` directory: before human task H2 vendors the
    /// fonts this returns an empty database (font resolution then reports every
    /// family as missing).
    pub fn with_bundled() -> FontDb {
        let mut db = FontDb::new();
        for (_stem, bytes) in super::bundled::bundled_fonts() {
            // Family comes from the font's own name table, not the file stem.
            db.add_sfnt(Arc::new(bytes.to_vec()), false, None);
        }
        db
    }

    /// A database with bundled fonts plus explicitly authorized native system
    /// font directories and any caller-owned `extra_dirs`.
    pub fn with_system(extra_dirs: &[PathBuf]) -> FontDb {
        FontDb::with_sources(extra_dirs, true)
    }

    /// Builds a database from bundled fonts, caller-owned directories, and—only
    /// when `include_system_fonts` is true—OS-managed font directories. On wasm
    /// only bundled and subsequently registered byte sources are available.
    pub fn with_sources(extra_dirs: &[PathBuf], include_system_fonts: bool) -> FontDb {
        // `db` is only mutated in the native scan below.
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut db = FontDb::with_bundled();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut dirs = Vec::new();
            if include_system_fonts {
                dirs.extend(SYSTEM_FONT_DIRS.iter().map(PathBuf::from));
                if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                    dirs.push(home.join("Library/Fonts"));
                    dirs.push(home.join(".fonts"));
                    dirs.push(home.join(".local/share/fonts"));
                }
            }
            dirs.extend(extra_dirs.iter().cloned());
            for dir in dirs {
                db.scan_dir(&dir);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (extra_dirs, include_system_fonts);
        db
    }

    /// Registers a font from raw bytes at runtime (wasm `register_font`). Unlike
    /// system fonts, a parse failure here is an error.
    pub fn register(&mut self, bytes: Vec<u8>) -> Result<()> {
        super::registration::validate_font(&bytes, None)?;
        let data = Arc::new(bytes);
        let declared = ttf_parser::fonts_in_collection(&data).unwrap_or(1).max(1) as usize;
        if self.faces.len().saturating_add(declared) > MAX_FONT_FACES {
            return Err(OfficeError::unsupported(
                crate::UnsupportedCode::DecompressionLimit,
                format!("font face count exceeds limit {MAX_FONT_FACES}"),
            ));
        }
        let added = self.add_sfnt(data, false, None);
        if added == 0 {
            return Err(OfficeError::Font("unparseable font bytes".into()));
        }
        Ok(())
    }

    /// Registers only one validated face from an SFNT collection.
    pub fn register_face(&mut self, bytes: Vec<u8>, face_index: u32) -> Result<()> {
        let identity = super::registration::validate_font_index(&bytes, face_index)?;
        if self.faces.len() >= MAX_FONT_FACES {
            return Err(OfficeError::unsupported(
                crate::UnsupportedCode::DecompressionLimit,
                format!("font face count exceeds limit {MAX_FONT_FACES}"),
            ));
        }
        let data = Arc::new(bytes);
        let face = ttf_parser::Face::parse(&data, face_index)
            .map_err(|_| OfficeError::Font("unparseable font bytes".into()))?;
        let family = face_name(&face, ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
            .or_else(|| face_name(&face, ttf_parser::name_id::FAMILY))
            .unwrap_or_else(|| "Unknown".to_string());
        self.faces.push(Face {
            family_name: family,
            full_name: face_name(&face, ttf_parser::name_id::FULL_NAME),
            subfamily: face_name(&face, ttf_parser::name_id::SUBFAMILY)
                .unwrap_or_else(|| "Regular".to_string()),
            weight: identity.weight,
            stretch: identity.stretch,
            italic: identity.italic,
            embedded: false,
            sha256: identity.sha256,
            hash_pinned: false,
            embedding_permission: identity.fs_type,
            data,
            index: face_index,
        });
        Ok(())
    }

    /// Registers a document-embedded face (top matching priority). The family
    /// hint from `fontTable.xml` is authoritative; bold/italic come from the
    /// embed slot rather than the (possibly stripped) name table.
    pub fn register_embedded(
        &mut self,
        family_hint: &str,
        bytes: Vec<u8>,
        bold: bool,
        italic: bool,
    ) {
        let data = Arc::new(bytes);
        let sha256 = sha256_hex(&data);
        // Try the real name table but override identity with the embed slot.
        let remaining = MAX_FONT_FACES.saturating_sub(self.faces.len()) as u32;
        let count = ttf_parser::fonts_in_collection(&data)
            .unwrap_or(1)
            .max(1)
            .min(remaining);
        let mut any = false;
        for index in 0..count {
            if ttf_parser::Face::parse(&data, index).is_ok() {
                let parsed = ttf_parser::Face::parse(&data, index).expect("face parsed above");
                let embedding_permission = super::registration::embedding_permission(&parsed);
                if !embedding_permission.allows_outline_embedding() {
                    continue;
                }
                self.faces.push(Face {
                    family_name: family_hint.to_string(),
                    full_name: None,
                    subfamily: style_name(bold, italic).to_string(),
                    weight: if bold { 700 } else { 400 },
                    stretch: 5,
                    italic,
                    embedded: true,
                    sha256: sha256.clone(),
                    hash_pinned: false,
                    embedding_permission,
                    data: data.clone(),
                    index,
                });
                any = true;
            }
        }
        if !any {
            // Even if parsing failed, keep the bytes for a best-effort embed.
            self.faces.push(Face {
                family_name: family_hint.to_string(),
                full_name: None,
                subfamily: style_name(bold, italic).to_string(),
                weight: if bold { 700 } else { 400 },
                stretch: 5,
                italic,
                embedded: true,
                sha256,
                hash_pinned: false,
                embedding_permission: super::registration::EmbeddingPermission::Unknown,
                data,
                index: 0,
            });
        }
    }

    /// All faces in the database.
    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// Looks up a face by id.
    pub fn face(&self, id: FaceId) -> Option<&Face> {
        self.faces.get(id.0)
    }

    /// Number of faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Whether the database is empty (e.g. bundled fonts not yet vendored).
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Selects exact-only or compatible family resolution for this database.
    pub fn set_resolution_mode(&mut self, mode: crate::ResolutionMode) {
        self.resolution_mode = mode;
    }

    /// The active exact-only or compatible family resolution policy.
    pub fn resolution_mode(&self) -> crate::ResolutionMode {
        self.resolution_mode
    }

    /// Records a text-free `fontTable.xml` classing hint for one authored name.
    pub(crate) fn set_classing_hint(&mut self, family: &str, hint: super::requests::FontClassing) {
        self.classing_hints
            .insert(family.trim().to_ascii_lowercase(), hint);
    }

    /// Returns the metadata-derived classing hint for an authored family.
    pub(crate) fn classing_hint(&self, family: &str) -> Option<&super::requests::FontClassing> {
        self.classing_hints.get(&family.trim().to_ascii_lowercase())
    }

    /// Marks registered faces whose complete artifacts were digest-pinned by
    /// the host, enabling reproducible exactness telemetry.
    pub fn mark_hash_pinned(&mut self, hashes: &[String]) {
        for face in &mut self.faces {
            face.hash_pinned |= hashes
                .iter()
                .any(|hash| hash.eq_ignore_ascii_case(&face.sha256));
        }
    }

    /// Parses `bytes` as a single font or a collection, pushing each face.
    /// Returns the number of faces added. `hint` overrides the family name.
    fn add_sfnt(&mut self, data: Arc<Vec<u8>>, embedded: bool, hint: Option<&str>) -> usize {
        let sha256 = sha256_hex(&data);
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1).max(1);
        let mut added = 0;
        for index in 0..count {
            let Ok(face) = ttf_parser::Face::parse(&data, index) else {
                continue;
            };
            let embedding_permission = super::registration::embedding_permission(&face);
            if !embedding_permission.allows_outline_embedding() {
                continue;
            }
            let family = hint
                .map(str::to_string)
                .or_else(|| face_name(&face, ttf_parser::name_id::TYPOGRAPHIC_FAMILY))
                .or_else(|| face_name(&face, ttf_parser::name_id::FAMILY))
                .unwrap_or_else(|| "Unknown".to_string());
            self.faces.push(Face {
                family_name: family,
                full_name: face_name(&face, ttf_parser::name_id::FULL_NAME),
                subfamily: face_name(&face, ttf_parser::name_id::SUBFAMILY)
                    .unwrap_or_else(|| "Regular".to_string()),
                weight: face.weight().to_number(),
                stretch: face.width().to_number(),
                italic: face.is_italic() || face.is_oblique(),
                embedded,
                sha256: sha256.clone(),
                hash_pinned: false,
                embedding_permission,
                data: data.clone(),
                index,
            });
            added += 1;
        }
        added
    }

    /// Non-recursively scans a directory for font files (native only).
    #[cfg(not(target_arch = "wasm32"))]
    fn scan_dir(&mut self, dir: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_font = matches!(
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("ttf") | Some("otf") | Some("ttc")
            );
            if !is_font {
                continue;
            }
            // System fonts are best-effort: silently skip unreadable/unparseable.
            if let Ok(bytes) = std::fs::read(&path) {
                self.add_sfnt(Arc::new(bytes), false, None);
            }
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn style_name(bold: bool, italic: bool) -> &'static str {
    match (bold, italic) {
        (true, true) => "Bold Italic",
        (true, false) => "Bold",
        (false, true) => "Italic",
        (false, false) => "Regular",
    }
}

/// Reads a name-table record as a readable string.
fn face_name(face: &ttf_parser::Face, id: u16) -> Option<String> {
    face.names()
        .into_iter()
        .filter(|n| n.name_id == id)
        .find_map(|n| n.to_string())
        .filter(|s| !s.is_empty())
}

/// Native system font directories scanned by [`FontDb::with_system`].
#[cfg(not(target_arch = "wasm32"))]
const SYSTEM_FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/Library/Fonts",
    "/System/Library/Fonts",
    "C:\\Windows\\Fonts",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// With the bundled fonts vendored (H2), the database loads all 20 faces
    /// with the expected families. Skips before the fonts are present.
    #[test]
    fn bundled_db_loads_expected_faces() {
        let db = FontDb::with_bundled();
        if db.is_empty() {
            eprintln!("skipping: no bundled fonts vendored (human task H2)");
            return;
        }
        assert_eq!(db.len(), 20, "expected 20 bundled faces");
        for family in [
            "Carlito",
            "Caladea",
            "Liberation Sans",
            "Liberation Serif",
            "Liberation Mono",
        ] {
            assert!(
                db.faces()
                    .iter()
                    .any(|f| f.family_name.eq_ignore_ascii_case(family)),
                "missing bundled family {family}"
            );
        }
    }

    #[test]
    fn explicit_font_directory_remains_authorized_when_system_scan_is_disabled() {
        let db = FontDb::with_sources(&[PathBuf::from("../pdf-core/wasm/pkg/fonts")], false);
        assert!(
            db.faces().iter().any(|face| {
                face.parsed()
                    .and_then(|parsed| parsed.glyph_index('\u{0634}'))
                    .is_some()
            }),
            "explicit directory did not load Arabic coverage: {:?}",
            db.faces()
                .iter()
                .map(|face| &face.family_name)
                .collect::<Vec<_>>()
        );
        assert!(
            db.faces().iter().any(|face| {
                face.parsed()
                    .and_then(|parsed| parsed.glyph_index('\u{65E5}'))
                    .is_some()
            }),
            "explicit directory did not load CJK coverage"
        );
    }
}
