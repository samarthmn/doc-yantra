//! ODTTF (obfuscated font) deobfuscation.
//!
//! DOCX/PPTX may embed fonts as `.odttf` parts — a TTF/OTF whose first 32 bytes
//! are XOR-obfuscated with the 16-byte GUID from the embed's `w:fontKey`. Two
//! byte-order interpretations exist in the wild; we try both and accept whichever
//! yields a valid sfnt magic.

use crate::{OfficeError, Result, UnsupportedCode};

/// Valid sfnt magics: TrueType (`00 01 00 00`), CFF (`OTTO`), Apple (`true`),
/// and collections (`ttcf`).
const SFNT_MAGICS: [[u8; 4]; 4] = [
    [0x00, 0x01, 0x00, 0x00],
    [0x4F, 0x54, 0x54, 0x4F], // OTTO
    [0x74, 0x72, 0x75, 0x65], // true
    [0x74, 0x74, 0x63, 0x66], // ttcf
];

/// Deobfuscates ODTTF `data` using the `w:fontKey` GUID, returning plain sfnt
/// bytes. Errors with `Unsupported{UnknownElement}` if neither interpretation
/// produces a valid font.
pub fn deobfuscate(font_key: &str, data: &[u8]) -> Result<Vec<u8>> {
    let hex: String = font_key.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return Err(OfficeError::Font(format!(
            "fontKey has {} hex digits, expected 32",
            hex.len()
        )));
    }
    let mut hb = [0u8; 16];
    for (i, byte) in hb.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| OfficeError::Font("fontKey is not valid hex".into()))?;
    }

    // Interpretation B: hex bytes in string order, reversed — the real-world
    // algorithm (python-docx, docx-rs). Tried first.
    let key_b = reversed(hb);
    // Interpretation A: formal GUID byte order (first three groups little-endian),
    // then reversed — a fallback for producers that disagree.
    //
    // NOTE: the two keys share their first 4 bytes, so the 4-byte sfnt magic
    // alone cannot disambiguate them; we prefer whichever yields a *parseable*
    // font and only fall back to the magic check for synthetic inputs.
    let guid_a = [
        hb[3], hb[2], hb[1], hb[0], hb[5], hb[4], hb[7], hb[6], hb[8], hb[9], hb[10], hb[11],
        hb[12], hb[13], hb[14], hb[15],
    ];
    let key_a = reversed(guid_a);

    // Prefer a candidate that parses as a real font.
    for key in [key_b, key_a] {
        let out = xor_prefix(data, &key);
        if ttf_parser::Face::parse(&out, 0).is_ok() {
            return Ok(out);
        }
    }
    // Fall back to the first candidate with a valid sfnt magic (synthetic data).
    for key in [key_b, key_a] {
        let out = xor_prefix(data, &key);
        if out.len() >= 4 && SFNT_MAGICS.contains(&[out[0], out[1], out[2], out[3]]) {
            return Ok(out);
        }
    }
    Err(OfficeError::unsupported(
        UnsupportedCode::UnknownElement,
        "odttf: no GUID interpretation yields a valid sfnt magic",
    ))
}

fn reversed(mut bytes: [u8; 16]) -> [u8; 16] {
    bytes.reverse();
    bytes
}

/// Copies `data`, XORing the first 32 bytes with `key[i % 16]`.
fn xor_prefix(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let mut out = data.to_vec();
    for (i, byte) in out.iter_mut().take(32).enumerate() {
        *byte ^= key[i % 16];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Obfuscate a fabricated sfnt with a known GUID, then round-trip it back.
    #[test]
    fn round_trips_interpretation_b() {
        // A 40-byte buffer whose first 4 bytes are the TrueType magic.
        let mut plain = vec![0u8; 40];
        plain[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        for (i, b) in plain.iter_mut().enumerate().skip(4) {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }

        let font_key = "{B1C2D3E4-F506-1728-3949-5A6B7C8D9E0F}";
        // Build interpretation-B key exactly as the deobfuscator will.
        let hex: String = font_key.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let mut hb = [0u8; 16];
        for (i, byte) in hb.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let key = reversed(hb);

        // Obfuscate: XOR the first 32 bytes with the key.
        let obf = xor_prefix(&plain, &key);
        assert_ne!(&obf[0..4], &[0x00, 0x01, 0x00, 0x00]);

        let recovered = deobfuscate(font_key, &obf).expect("deobfuscate");
        assert_eq!(recovered, plain);
    }

    #[test]
    fn rejects_bad_key_length() {
        assert!(deobfuscate("{1234}", &[0u8; 40]).is_err());
    }

    #[test]
    fn rejects_garbage() {
        // Random bytes under a valid-length key never produce a valid magic.
        let key = "{00000000-0000-0000-0000-000000000000}";
        let data = vec![0xAB_u8; 64];
        assert!(deobfuscate(key, &data).is_err());
    }
}
