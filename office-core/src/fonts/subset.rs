//! Font subsetting for PDF embedding.
//!
//! Subsets a face to exactly its used glyphs via the `subsetter` crate, keeping
//! the old→new glyph-id mapping so the emitter can write CIDs = new gids under
//! `Identity-H` / `CIDToGIDMap Identity`, plus per-glyph widths and a ToUnicode
//! table. For TrueType faces, compact remapping must retain composite closure,
//! global hint tables, and the instructions on retained simple and composite
//! glyphs; `pdf_font_embedding_test` locks that production contract down.

use super::db::Face;
use super::metrics::FaceMetrics;
use crate::profile::Warning;
use crate::{OfficeError, Result};
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::GlyphId;

/// A subset font ready for PDF embedding.
#[derive(Debug, Clone)]
pub struct SubsetFont {
    /// The subset (or, on fallback, full) sfnt bytes for `FontFile2`/`FontFile3`.
    pub data: Vec<u8>,
    /// Old glyph id → CID (new glyph id). Complete over the used set.
    pub remap: BTreeMap<u16, u16>,
    /// `(CID, advance in 1000-unit em space)` for the PDF `/W` array.
    pub widths: Vec<(u16, u16)>,
    /// `(CID, unicode string)` for the ToUnicode CMap.
    pub to_unicode: Vec<(u16, String)>,
    /// True for CFF-flavoured (OTTO) fonts (emit as `FontFile3`).
    pub is_cff: bool,
    /// Set when subsetting failed and the full font was embedded instead.
    pub warning: Option<Warning>,
}

/// Subsets `face` to `used` glyphs (`used_chars` maps each glyph to the source
/// character for ToUnicode). Falls back to embedding the full font (with a
/// `font_not_subset` warning) if the subsetter rejects the face.
pub fn subset_for_pdf(
    face: &Face,
    used: &BTreeSet<GlyphId>,
    used_chars: &BTreeMap<GlyphId, char>,
) -> Result<SubsetFont> {
    let parsed = face
        .parsed()
        .ok_or_else(|| OfficeError::Font(format!("cannot parse face {}", face.family_name)))?;
    let metrics = FaceMetrics::new(parsed);
    let is_cff = face.data.len() >= 4 && &face.data[..4] == b"OTTO";

    // Glyph set always includes .notdef (gid 0).
    let mut gids: Vec<u16> = std::iter::once(0).chain(used.iter().map(|g| g.0)).collect();
    gids.sort_unstable();
    gids.dedup();

    if !face.embedding_permission.allows_outline_embedding() {
        return Err(OfficeError::unsupported(
            crate::UnsupportedCode::FontLicenseRestricted,
            format!(
                "font {} has embedding policy {:?}",
                face.family_name, face.embedding_permission
            ),
        ));
    }
    if !face.embedding_permission.allows_subsetting() {
        const MAX_WHOLE_FONT_BYTES: usize = 20 * 1024 * 1024;
        if face.data.len() > MAX_WHOLE_FONT_BYTES || face.data.starts_with(b"ttcf") {
            return Err(OfficeError::unsupported(
                crate::UnsupportedCode::FontLicenseRestricted,
                format!(
                    "font {} forbids subsetting and cannot be embedded whole within policy",
                    face.family_name
                ),
            ));
        }
        let remap: BTreeMap<u16, u16> = gids.iter().map(|&glyph| (glyph, glyph)).collect();
        return Ok(build(
            face.data.as_ref().clone(),
            remap,
            &metrics,
            used_chars,
            is_cff,
            Some(Warning::new(
                "font_embedded_whole",
                face.family_name.clone(),
            )),
        ));
    }

    let mapper = subsetter::GlyphRemapper::new_from_glyphs(&gids);
    match subsetter::subset(&face.data, face.index, &mapper) {
        Ok(data) => {
            let remap: BTreeMap<u16, u16> = gids
                .iter()
                .filter_map(|&old| mapper.get(old).map(|new| (old, new)))
                .collect();
            Ok(build(data, remap, &metrics, used_chars, is_cff, None))
        }
        Err(_) => {
            // Fallback: embed the full font with an identity CID = GID mapping.
            let remap: BTreeMap<u16, u16> = gids.iter().map(|&g| (g, g)).collect();
            Ok(build(
                face.data.as_ref().clone(),
                remap,
                &metrics,
                used_chars,
                is_cff,
                Some(Warning::new("font_not_subset", face.family_name.clone())),
            ))
        }
    }
}

impl SubsetFont {
    /// Bare `CFF ` table bytes for `FontFile3` embedding. `None` when the
    /// program is not CFF-flavoured or the table cannot be located.
    pub fn cff_table(&self) -> Option<Vec<u8>> {
        if !self.is_cff {
            return None;
        }
        let face = ttf_parser::Face::parse(&self.data, 0).ok()?;
        face.raw_face()
            .table(ttf_parser::Tag::from_bytes(b"CFF "))
            .map(<[u8]>::to_vec)
    }
}

fn build(
    data: Vec<u8>,
    remap: BTreeMap<u16, u16>,
    metrics: &FaceMetrics,
    used_chars: &BTreeMap<GlyphId, char>,
    is_cff: bool,
    warning: Option<Warning>,
) -> SubsetFont {
    let widths: Vec<(u16, u16)> = remap
        .iter()
        .map(|(&old, &cid)| (cid, metrics.advance_1000(GlyphId(old))))
        .collect();
    let to_unicode: Vec<(u16, String)> = used_chars
        .iter()
        .filter_map(|(gid, ch)| remap.get(&gid.0).map(|&cid| (cid, ch.to_string())))
        .collect();
    SubsetFont {
        data,
        remap,
        widths,
        to_unicode,
        is_cff,
        warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::db::FontDb;

    /// Subsets a bundled face when fonts are vendored; skips otherwise (H2).
    #[test]
    fn subsets_a_bundled_face_when_available() {
        let db = FontDb::with_bundled();
        let Some(face) = db.faces().first() else {
            eprintln!("skipping: no bundled fonts vendored (human task H2)");
            return;
        };
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let mut used = BTreeSet::new();
        let mut chars = BTreeMap::new();
        for ch in ['H', 'e', 'l', 'o'] {
            if let Some(g) = metrics.glyph_id(ch) {
                used.insert(g);
                chars.insert(g, ch);
            }
        }
        let subset = subset_for_pdf(face, &used, &chars).expect("subset");
        assert!(!subset.data.is_empty());
        assert!(subset.remap.contains_key(&0), ".notdef retained");
        assert!(subset.widths.iter().all(|&(_, w)| w <= 3000));
    }

    #[test]
    fn no_subsetting_policy_embeds_the_complete_sfnt() {
        let db = FontDb::with_bundled();
        let Some(source) = db.faces().first() else {
            return;
        };
        let mut face = source.clone();
        face.embedding_permission =
            crate::fonts::registration::EmbeddingPermission::InstallableNoSubsetting;
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let glyph = metrics.glyph_id('A').expect("A glyph");
        let subset = subset_for_pdf(
            &face,
            &BTreeSet::from([glyph]),
            &BTreeMap::from([(glyph, 'A')]),
        )
        .expect("whole-font embedding");

        assert_eq!(subset.data.len(), face.data.len());
        assert_eq!(subset.remap.get(&glyph.0), Some(&glyph.0));
        assert_eq!(
            subset.warning.as_ref().map(|warning| warning.code),
            Some("font_embedded_whole")
        );
    }
}
