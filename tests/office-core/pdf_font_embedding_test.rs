use office_core::fonts::{FaceMetrics, FontDb, subset_for_pdf};
use std::collections::{BTreeMap, BTreeSet};

const CARLITO: &[u8] = include_bytes!("../../office-core/fonts/carlito/Carlito-Regular.ttf");

#[test]
fn compact_type0_subset_retains_truetype_hint_program_and_composite_closure() {
    let mut db = FontDb::new();
    db.register(CARLITO.to_vec()).expect("register hinted face");
    let face = db.faces().first().expect("registered face");
    let metrics = FaceMetrics::new(face.parsed().expect("parse hinted face"));
    let simple = metrics.glyph_id('A').expect("A glyph");
    let composite = metrics.glyph_id('é').expect("e-acute glyph");
    let used = BTreeSet::from([simple, composite]);
    let characters = BTreeMap::from([(simple, 'A'), (composite, 'é')]);

    let subset = subset_for_pdf(face, &used, &characters).expect("compact hinted subset");

    assert!(!subset.is_cff, "fixture must exercise the TrueType path");
    assert_eq!(subset.warning, None, "fixture must be compactly subset");
    assert!(
        subset.remap[&simple.0] < simple.0 && subset.remap[&composite.0] < composite.0,
        "used glyph IDs must be compactly remapped"
    );
    for tag in [*b"fpgm", *b"prep", *b"cvt "] {
        assert_eq!(
            sfnt_table(&subset.data, tag),
            sfnt_table(CARLITO, tag),
            "global hint table {:?} must survive byte-for-byte",
            std::str::from_utf8(&tag).expect("ASCII tag")
        );
    }

    let source_simple = glyph_data(CARLITO, simple.0).expect("source simple glyph");
    let subset_simple =
        glyph_data(&subset.data, subset.remap[&simple.0]).expect("subset simple glyph");
    assert_eq!(
        glyph_instructions(source_simple),
        glyph_instructions(subset_simple),
        "simple-glyph instructions must survive remapping"
    );
    assert!(!glyph_instructions(subset_simple).is_empty());

    let source_composite = glyph_data(CARLITO, composite.0).expect("source composite glyph");
    let subset_composite =
        glyph_data(&subset.data, subset.remap[&composite.0]).expect("subset composite glyph");
    assert_eq!(
        glyph_instructions(source_composite),
        glyph_instructions(subset_composite),
        "composite-glyph instructions must survive component remapping"
    );
    assert!(!glyph_instructions(subset_composite).is_empty());

    let glyph_slots = sfnt_num_glyphs(&subset.data).expect("subset maxp glyph count");
    let components = composite_components(subset_composite).expect("subset components");
    assert!(!components.is_empty(), "fixture must remain composite");
    assert!(
        components.iter().all(|gid| *gid < glyph_slots),
        "every remapped component must resolve inside the compact subset"
    );
    assert!(
        usize::from(glyph_slots) > subset.remap.len(),
        "composite dependencies must extend the explicit used-glyph set"
    );
    ttf_parser::Face::parse(&subset.data, 0).expect("subset must remain a valid sfnt");
    assert_eq!(sfnt_checksum(&subset.data), 0xB1B0_AFBA);
}

fn sfnt_table(data: &[u8], tag: [u8; 4]) -> Option<&[u8]> {
    let count = usize::from(read_u16(data, 4)?);
    (0..count).find_map(|index| {
        let record = 12 + index * 16;
        (data.get(record..record + 4)? == tag).then(|| {
            let offset = read_u32(data, record + 8)? as usize;
            let length = read_u32(data, record + 12)? as usize;
            data.get(offset..offset.checked_add(length)?)
        })?
    })
}

fn sfnt_num_glyphs(data: &[u8]) -> Option<u16> {
    read_u16(sfnt_table(data, *b"maxp")?, 4)
}

fn glyph_data(data: &[u8], gid: u16) -> Option<&[u8]> {
    let head = sfnt_table(data, *b"head")?;
    let loca = sfnt_table(data, *b"loca")?;
    let glyf = sfnt_table(data, *b"glyf")?;
    let long_loca = read_i16(head, 50)? != 0;
    let offset = |index: usize| {
        if long_loca {
            read_u32(loca, index.checked_mul(4)?).and_then(|value| usize::try_from(value).ok())
        } else {
            read_u16(loca, index.checked_mul(2)?).map(|value| usize::from(value) * 2)
        }
    };
    let start = offset(usize::from(gid))?;
    let end = offset(usize::from(gid) + 1)?;
    glyf.get(start..end)
}

fn glyph_instructions(data: &[u8]) -> &[u8] {
    let Some(contours) = read_i16(data, 0) else {
        return &[];
    };
    if contours >= 0 {
        let Some(length_offset) = 10usize.checked_add(contours as usize * 2) else {
            return &[];
        };
        let Some(length) = read_u16(data, length_offset).map(usize::from) else {
            return &[];
        };
        return data
            .get(length_offset + 2..length_offset + 2 + length)
            .unwrap_or_default();
    }
    composite_tail(data)
        .and_then(|(flags, offset)| {
            (flags & 0x0100 != 0).then(|| {
                let length = usize::from(read_u16(data, offset)?);
                data.get(offset + 2..offset + 2 + length)
            })?
        })
        .unwrap_or_default()
}

fn composite_components(data: &[u8]) -> Option<Vec<u16>> {
    (read_i16(data, 0)? < 0).then_some(())?;
    let mut offset = 10;
    let mut components = Vec::new();
    loop {
        let flags = read_u16(data, offset)?;
        components.push(read_u16(data, offset + 2)?);
        offset = component_end(offset, flags)?;
        if flags & 0x0020 == 0 {
            return Some(components);
        }
    }
}

fn composite_tail(data: &[u8]) -> Option<(u16, usize)> {
    (read_i16(data, 0)? < 0).then_some(())?;
    let mut offset = 10;
    loop {
        let flags = read_u16(data, offset)?;
        offset = component_end(offset, flags)?;
        if flags & 0x0020 == 0 {
            return Some((flags, offset));
        }
    }
}

fn component_end(offset: usize, flags: u16) -> Option<usize> {
    let arguments = if flags & 0x0001 != 0 { 4 } else { 2 };
    let transform = if flags & 0x0008 != 0 {
        2
    } else if flags & 0x0040 != 0 {
        4
    } else if flags & 0x0080 != 0 {
        8
    } else {
        0
    };
    offset.checked_add(4 + arguments + transform)
}

fn sfnt_checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut bytes = [0; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(bytes))
    })
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_i16(data: &[u8], offset: usize) -> Option<i16> {
    Some(i16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
