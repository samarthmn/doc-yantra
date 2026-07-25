//! Bare-CFF CID-addressing validation for PDF embedding.
//!
//! The emitter addresses CFF glyphs as `CID == glyph index`: the `subsetter`
//! crate rewrites every subset charset to that identity, and the embedded
//! `/Encoding` CMap maps content codes to those CIDs. Whole-font embeds
//! (faces whose license forbids subsetting, or faces the subsetter rejects)
//! bypass that rewrite, so their charset can map glyphs to arbitrary CIDs.
//! Under PDF 32000-1 §9.7.4.2 a viewer resolves CIDs through that charset for
//! CID-keyed programs (name-keyed programs use CIDs as glyph indices
//! directly), so a non-identity charset would make every viewer paint glyphs
//! the emitter never selected. That must fail closed, not ship.

use crate::{OfficeError, Result, UnsupportedCode};

/// Returns `Ok(())` when `CID == glyph index` holds for the program: either
/// the CFF is name-keyed (no `ROS`), or it is CID-keyed and its charset maps
/// every glyph id to itself. Malformed programs and non-identity charsets are
/// errors.
pub(crate) fn ensure_identity_cid_addressing(cff: &[u8]) -> Result<()> {
    Parser { cff }.run()
}

struct Parser<'a> {
    cff: &'a [u8],
}

impl Parser<'_> {
    fn run(&self) -> Result<()> {
        let header_size = usize::from(*self.byte(2)?);
        let name_index_end = self.skip_index(header_size)?;
        let (top_dict_start, top_dict_end) = self.first_index_entry(name_index_end)?;
        let top = parse_top_dict(self.slice(top_dict_start, top_dict_end)?)?;

        if !top.has_ros {
            // Name-keyed program: CIDs address glyph indices directly.
            return Ok(());
        }
        let charstrings_offset = top
            .charstrings
            .ok_or_else(|| malformed("CID-keyed CFF has no CharStrings operator"))?;
        let glyph_count = usize::from(self.index_count(charstrings_offset)?);
        let charset_offset = top
            .charset
            .ok_or_else(|| malformed("CID-keyed CFF has no charset operator"))?;
        if charset_offset <= 2 {
            // 0/1/2 select predefined name-keyed charsets (ISOAdobe/Expert);
            // they never encode the identity CID mapping of a CID-keyed font.
            return Err(non_identity(
                "CID-keyed CFF uses a predefined charset instead of an identity mapping",
            ));
        }
        self.verify_identity_charset(charset_offset, glyph_count)
    }

    fn byte(&self, at: usize) -> Result<&u8> {
        self.cff.get(at).ok_or_else(|| malformed("truncated CFF"))
    }

    fn slice(&self, start: usize, end: usize) -> Result<&[u8]> {
        if start > end {
            return Err(malformed("CFF region has negative length"));
        }
        self.cff
            .get(start..end)
            .ok_or_else(|| malformed("truncated CFF"))
    }

    fn u16_at(&self, at: usize) -> Result<u16> {
        let bytes = self.slice(at, at + 2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn index_count(&self, at: usize) -> Result<u16> {
        self.u16_at(at)
    }

    /// Returns the byte offset just past the INDEX starting at `at`.
    fn skip_index(&self, at: usize) -> Result<usize> {
        let count = usize::from(self.u16_at(at)?);
        if count == 0 {
            return Ok(at + 2);
        }
        let off_size = usize::from(*self.byte(at + 2)?);
        if !(1..=4).contains(&off_size) {
            return Err(malformed("CFF INDEX offSize outside 1..=4"));
        }
        let offsets_at = at + 3;
        let data_at = offsets_at + (count + 1) * off_size;
        let last = self.offset_at(offsets_at + count * off_size, off_size)?;
        Ok(data_at + last - 1)
    }

    /// Start/end (absolute) of the first entry of the INDEX at `at`.
    fn first_index_entry(&self, at: usize) -> Result<(usize, usize)> {
        let count = usize::from(self.u16_at(at)?);
        if count == 0 {
            return Err(malformed("CFF INDEX is empty"));
        }
        let off_size = usize::from(*self.byte(at + 2)?);
        if !(1..=4).contains(&off_size) {
            return Err(malformed("CFF INDEX offSize outside 1..=4"));
        }
        let offsets_at = at + 3;
        let data_at = offsets_at + (count + 1) * off_size;
        let first = self.offset_at(offsets_at, off_size)?;
        let second = self.offset_at(offsets_at + off_size, off_size)?;
        Ok((data_at + first - 1, data_at + second - 1))
    }

    fn offset_at(&self, at: usize, off_size: usize) -> Result<usize> {
        let bytes = self.slice(at, at + off_size)?;
        let mut value = 0usize;
        for &b in bytes {
            value = (value << 8) | usize::from(b);
        }
        if value == 0 {
            return Err(malformed("CFF INDEX offset is zero"));
        }
        Ok(value)
    }

    fn verify_identity_charset(&self, at: usize, glyph_count: usize) -> Result<()> {
        // Charset covers glyphs 1..glyph_count (.notdef is implicit).
        let format = *self.byte(at)?;
        match format {
            0 => {
                for gid in 1..glyph_count {
                    let cid = usize::from(self.u16_at(at + 1 + (gid - 1) * 2)?);
                    if cid != gid {
                        return Err(non_identity(
                            "CID-keyed CFF charset (format 0) is not the identity mapping",
                        ));
                    }
                }
                Ok(())
            }
            1 | 2 => {
                let mut cursor = at + 1;
                let mut gid = 1usize;
                while gid < glyph_count {
                    let first = usize::from(self.u16_at(cursor)?);
                    let n_left = if format == 1 {
                        usize::from(*self.byte(cursor + 2)?)
                    } else {
                        usize::from(self.u16_at(cursor + 2)?)
                    };
                    cursor += if format == 1 { 3 } else { 4 };
                    if first != gid {
                        return Err(non_identity(
                            "CID-keyed CFF charset range does not continue the identity mapping",
                        ));
                    }
                    gid += n_left + 1;
                }
                Ok(())
            }
            _ => Err(malformed("CFF charset format outside 0..=2")),
        }
    }
}

struct TopDict {
    has_ros: bool,
    charset: Option<usize>,
    charstrings: Option<usize>,
}

/// Minimal CFF DICT scan: collects the `ROS`, `charset`, and `CharStrings`
/// operators; skips every operand type defined by the CFF spec.
fn parse_top_dict(data: &[u8]) -> Result<TopDict> {
    let mut top = TopDict {
        has_ros: false,
        charset: None,
        charstrings: None,
    };
    let mut operands: Vec<i64> = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let b0 = data[i];
        match b0 {
            32..=246 => {
                operands.push(i64::from(b0) - 139);
                i += 1;
            }
            247..=250 => {
                let b1 = *data.get(i + 1).ok_or_else(|| malformed("truncated DICT"))?;
                operands.push((i64::from(b0) - 247) * 256 + i64::from(b1) + 108);
                i += 2;
            }
            251..=254 => {
                let b1 = *data.get(i + 1).ok_or_else(|| malformed("truncated DICT"))?;
                operands.push(-(i64::from(b0) - 251) * 256 - i64::from(b1) - 108);
                i += 2;
            }
            28 => {
                let bytes = data
                    .get(i + 1..i + 3)
                    .ok_or_else(|| malformed("truncated DICT"))?;
                operands.push(i64::from(i16::from_be_bytes([bytes[0], bytes[1]])));
                i += 3;
            }
            29 => {
                let bytes = data
                    .get(i + 1..i + 5)
                    .ok_or_else(|| malformed("truncated DICT"))?;
                operands.push(i64::from(i32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ])));
                i += 5;
            }
            30 => {
                // Real number: nibbles until 0xF terminator. Value unused here.
                i += 1;
                loop {
                    let nibbles = *data.get(i).ok_or_else(|| malformed("truncated DICT"))?;
                    i += 1;
                    if nibbles & 0x0F == 0x0F || nibbles >> 4 == 0x0F {
                        break;
                    }
                }
                operands.push(0);
            }
            12 => {
                let op2 = *data.get(i + 1).ok_or_else(|| malformed("truncated DICT"))?;
                if op2 == 30 {
                    top.has_ros = true;
                }
                operands.clear();
                i += 2;
            }
            0..=21 => {
                match b0 {
                    15 => top.charset = operand_offset(&operands)?,
                    17 => top.charstrings = operand_offset(&operands)?,
                    _ => {}
                }
                operands.clear();
                i += 1;
            }
            _ => return Err(malformed("reserved CFF DICT byte")),
        }
    }
    Ok(top)
}

fn operand_offset(operands: &[i64]) -> Result<Option<usize>> {
    let &value = operands
        .last()
        .ok_or_else(|| malformed("CFF DICT operator without operand"))?;
    usize::try_from(value)
        .map(Some)
        .map_err(|_| malformed("negative CFF DICT offset"))
}

fn malformed(what: &str) -> OfficeError {
    OfficeError::Font(format!("{what}; refusing to embed an unverifiable CFF"))
}

fn non_identity(what: &str) -> OfficeError {
    OfficeError::unsupported(
        UnsupportedCode::MissingGlyph,
        format!("{what}; glyphs cannot be addressed as CID == glyph id"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal CID-keyed CFF: header, empty-ish INDEXes, a Top DICT
    /// with ROS + charset + CharStrings offsets, a CharStrings INDEX with
    /// `glyphs` entries, and a format-0 charset listing `cids`.
    fn synthetic_cid_cff(cids: &[u16]) -> Vec<u8> {
        let glyphs = cids.len() + 1; // + .notdef
        // Layout (offsets computed in order):
        // 0: header (4)
        // 4: Name INDEX: count=1, offSize=1, offsets [1, 2], data "A" => 4+3+2+1
        let header = [1u8, 0, 4, 4];
        let name_index = [0u8, 1, 1, 1, 2, b'A'];
        // Top DICT is built later (fixed-width operands so its size is stable):
        // each offset operand encoded as 29 (5 bytes), ROS operands 3 ints.
        // Top DICT INDEX header: count=1, offSize=1, offsets[1, 1+len]
        let top_dict_len = 5 + 5 + 5 + 2 /* ROS */ + 5 + 1 /* charset */ + 5 + 1 /* CharStrings */;
        let top_index_len = 2 + 1 + 2 + top_dict_len;
        let string_index = [0u8, 0];
        let gsubr_index = [0u8, 0];
        let charset_at = 4 + name_index.len() + top_index_len + 2 + 2;
        let charset_len = 1 + cids.len() * 2;
        let charstrings_at = charset_at + charset_len;

        let mut top_dict = Vec::new();
        for value in [0i32, 0, 0] {
            top_dict.push(29);
            top_dict.extend_from_slice(&value.to_be_bytes());
        }
        top_dict.extend_from_slice(&[12, 30]); // ROS
        top_dict.push(29);
        top_dict.extend_from_slice(&(charset_at as i32).to_be_bytes());
        top_dict.push(15); // charset
        top_dict.push(29);
        top_dict.extend_from_slice(&(charstrings_at as i32).to_be_bytes());
        top_dict.push(17); // CharStrings
        assert_eq!(top_dict.len(), top_dict_len);

        let mut cff = Vec::new();
        cff.extend_from_slice(&header);
        cff.extend_from_slice(&name_index);
        cff.extend_from_slice(&[0, 1, 1, 1, (1 + top_dict_len) as u8]);
        cff.extend_from_slice(&top_dict);
        cff.extend_from_slice(&string_index);
        cff.extend_from_slice(&gsubr_index);
        assert_eq!(cff.len(), charset_at);
        cff.push(0); // charset format 0
        for &cid in cids {
            cff.extend_from_slice(&cid.to_be_bytes());
        }
        assert_eq!(cff.len(), charstrings_at);
        // CharStrings INDEX: `glyphs` one-byte charstrings (0x0e endchar).
        cff.extend_from_slice(&(glyphs as u16).to_be_bytes());
        cff.push(1); // offSize
        for i in 0..=glyphs {
            cff.push((1 + i) as u8);
        }
        cff.extend(std::iter::repeat_n(0x0eu8, glyphs));
        cff
    }

    #[test]
    fn identity_charset_passes() {
        let cff = synthetic_cid_cff(&[1, 2, 3]);
        ensure_identity_cid_addressing(&cff).expect("identity charset is embeddable");
    }

    #[test]
    fn non_identity_charset_fails_closed_with_typed_error() {
        let cff = synthetic_cid_cff(&[57, 102, 833]); // preserved original CIDs
        let error = ensure_identity_cid_addressing(&cff)
            .expect_err("non-identity CID charset must fail closed");
        assert!(
            error.to_string().starts_with("unsupported:"),
            "expected a typed unsupported error, got: {error}"
        );
    }

    #[test]
    fn truncated_cff_is_rejected() {
        let cff = synthetic_cid_cff(&[1, 2, 3]);
        assert!(ensure_identity_cid_addressing(&cff[..cff.len() / 2]).is_err());
    }

    #[test]
    fn real_subsetter_output_passes() {
        // The production path: subsetter 0.2.6 rewrites every charset to the
        // identity, so the repo-committed Noto subset must validate.
        use crate::fonts::db::FontDb;
        use crate::fonts::metrics::FaceMetrics;
        use std::collections::{BTreeMap, BTreeSet};
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pdf-core/wasm/pkg/fonts/NotoSansCJKjp-Regular.otf"
        ))
        .expect("repo-committed Noto Sans CJK JP OTF");
        let mut db = FontDb::with_bundled();
        db.register(bytes).expect("register CFF face");
        let face = db.faces().last().expect("registered face");
        let metrics = FaceMetrics::new(face.parsed().unwrap());
        let mut used = BTreeSet::new();
        for ch in ['明', '細', '書'] {
            used.insert(metrics.glyph_id(ch).expect("glyph"));
        }
        let subset = crate::fonts::subset::subset_for_pdf(face, &used, &BTreeMap::new())
            .expect("subset CFF face");
        let cff = subset.cff_table().expect("bare CFF table");
        ensure_identity_cid_addressing(&cff).expect("subsetter output is identity-addressed");
    }
}
