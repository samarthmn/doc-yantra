//! The font database: a set of parsed faces sourced from bundled OFL fonts,
//! (native) system directories, runtime registration, and the document's own
//! embedded fonts.

use crate::{OfficeError, Result};
use std::path::PathBuf;
use std::sync::Arc;

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
    /// Whether the face is italic/oblique.
    pub italic: bool,
    /// Whether this face came from the document's embedded fonts (top priority).
    pub embedded: bool,
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
}

/// A collection of faces with resolution (see [`super::matching`]).
#[derive(Debug, Clone, Default)]
pub struct FontDb {
    faces: Vec<Face>,
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

    /// A database with the bundled fonts plus native system font directories and
    /// any `extra_dirs`. On wasm this is identical to [`FontDb::with_bundled`].
    pub fn with_system(extra_dirs: &[PathBuf]) -> FontDb {
        // `db` is only mutated in the native scan below.
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut db = FontDb::with_bundled();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut dirs: Vec<PathBuf> = SYSTEM_FONT_DIRS.iter().map(PathBuf::from).collect();
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                dirs.push(home.join("Library/Fonts"));
                dirs.push(home.join(".fonts"));
                dirs.push(home.join(".local/share/fonts"));
            }
            dirs.extend(extra_dirs.iter().cloned());
            for dir in dirs {
                db.scan_dir(&dir);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = extra_dirs;
        db
    }

    /// Registers a font from raw bytes at runtime (wasm `register_font`). Unlike
    /// system fonts, a parse failure here is an error.
    pub fn register(&mut self, bytes: Vec<u8>) -> Result<()> {
        let data = Arc::new(bytes);
        let added = self.add_sfnt(data, false, None);
        if added == 0 {
            return Err(OfficeError::Font("unparseable font bytes".into()));
        }
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
        // Try the real name table but override identity with the embed slot.
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1).max(1);
        let mut any = false;
        for index in 0..count {
            if ttf_parser::Face::parse(&data, index).is_ok() {
                self.faces.push(Face {
                    family_name: family_hint.to_string(),
                    full_name: None,
                    subfamily: style_name(bold, italic).to_string(),
                    weight: if bold { 700 } else { 400 },
                    italic,
                    embedded: true,
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
                italic,
                embedded: true,
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

    /// Parses `bytes` as a single font or a collection, pushing each face.
    /// Returns the number of faces added. `hint` overrides the family name.
    fn add_sfnt(&mut self, data: Arc<Vec<u8>>, embedded: bool, hint: Option<&str>) -> usize {
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1).max(1);
        let mut added = 0;
        for index in 0..count {
            let Ok(face) = ttf_parser::Face::parse(&data, index) else {
                continue;
            };
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
                italic: face.is_italic() || face.is_oblique(),
                embedded,
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
}
