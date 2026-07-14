//! Shared, PDF-agnostic Unicode itemization, coverage, fallback, and shaping.

use std::collections::HashSet;
use std::ops::Range;

use harfrust::{
    Direction as HbDirection, Feature, FontRef, Script as HbScript, ShapeOptions, ShaperData, Tag,
    UnicodeBuffer,
};
use unicode_bidi::{BidiInfo, Level};
use unicode_script::{Script, UnicodeScript};

/// A parsed OpenType face borrowed from caller-owned bytes.
#[derive(Clone)]
pub struct Face<'a> {
    bytes: &'a [u8],
    index: u32,
    units_per_em: u16,
}

impl<'a> Face<'a> {
    /// Parses one face from an sfnt or collection.
    pub fn new(bytes: &'a [u8], index: u32) -> Result<Self, ShapeError> {
        let parsed = ttf_parser::Face::parse(bytes, index).map_err(|_| ShapeError::InvalidFont)?;
        FontRef::from_index(bytes, index).map_err(|_| ShapeError::InvalidFont)?;
        Ok(Self {
            bytes,
            index,
            units_per_em: parsed.units_per_em(),
        })
    }

    /// The face's units per em, used to convert shaped font units to points.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Returns true when the cmap contains a non-zero glyph for `ch`.
    pub fn covers(&self, ch: char) -> bool {
        ttf_parser::Face::parse(self.bytes, self.index)
            .ok()
            .and_then(|face| face.glyph_index(ch))
            .is_some_and(|gid| gid.0 != 0)
    }
}

/// Horizontal text direction after bidi resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// A visual-order paragraph run with one script and resolved direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemizedRun {
    /// Source byte range in the original paragraph.
    pub range: Range<usize>,
    /// Source text in logical order; the run itself is placed in visual order.
    pub text: String,
    /// Resolved horizontal direction.
    pub direction: Direction,
    /// ISO 15924 script tag.
    pub script: [u8; 4],
}

/// One positioned glyph in font units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub cluster: usize,
    /// Byte range in `ShapedRun::source` represented by this glyph in
    /// extracted text. Continuation glyphs in the same semantic cluster use
    /// `None` so consumers can map the source text exactly once.
    pub unicode_range: Option<(usize, usize)>,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub face_index: usize,
}

/// A shaped run using one face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedRun {
    pub source: String,
    pub source_range: Range<usize>,
    pub direction: Direction,
    pub script: [u8; 4],
    pub face_index: usize,
    pub units_per_em: u16,
    pub glyphs: Vec<ShapedGlyph>,
    pub advance: i32,
}

/// Typed shaping failures shared by consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeError {
    InvalidFont,
    MissingGlyphs(Vec<char>),
    InvalidCluster,
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFont => f.write_str("invalid font"),
            Self::MissingGlyphs(chars) => write!(
                f,
                "missing glyphs: {}",
                chars
                    .iter()
                    .map(|ch| format!("U+{:04X}", *ch as u32))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::InvalidCluster => f.write_str("shaper returned an invalid cluster"),
        }
    }
}

impl std::error::Error for ShapeError {}

/// Optional OpenType feature overrides. Features left as `None` use HarfRust's
/// defaults, while an explicit boolean emits an OpenType feature setting.
/// Required shaping features such as `rlig`, Indic substitutions, and mark
/// positioning are intentionally not configurable here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeatureConfig {
    kern: Option<bool>,
    liga: Option<bool>,
}

impl FeatureConfig {
    /// Explicitly enables or disables OpenType `kern`.
    pub const fn kern(mut self, enabled: bool) -> Self {
        self.kern = Some(enabled);
        self
    }

    /// Explicitly enables or disables standard OpenType `liga`.
    pub const fn liga(mut self, enabled: bool) -> Self {
        self.liga = Some(enabled);
        self
    }

    fn harfrust_features(self) -> Vec<Feature> {
        let mut features = Vec::with_capacity(2);
        if let Some(enabled) = self.kern {
            features.push(Feature::new(Tag::new(b"kern"), u32::from(enabled), ..));
        }
        if let Some(enabled) = self.liga {
            features.push(Feature::new(Tag::new(b"liga"), u32::from(enabled), ..));
        }
        features
    }
}

/// Itemizes one paragraph into visual-order bidi runs, split by Unicode script.
pub fn itemize(text: &str) -> Vec<ItemizedRun> {
    if text.is_empty() {
        return Vec::new();
    }
    let bidi = BidiInfo::new(text, None);
    let mut out = Vec::new();
    for para in &bidi.paragraphs {
        let (_, visual) = bidi.visual_runs(para, para.range.clone());
        for range in visual {
            let direction = if bidi.levels[range.start].is_rtl() {
                Direction::Rtl
            } else {
                Direction::Ltr
            };
            split_script_runs(text, range, direction, &mut out);
        }
    }
    out
}

/// Returns `text` in Unicode bidi display order, preserving paragraph
/// separators.
///
/// This applies UAX #9 line reordering to each paragraph. It does not apply
/// glyph shaping, combining-mark reordering, or mirrored-glyph substitution.
pub fn bidi_visual_order(text: &str) -> String {
    bidi_visual_order_with_base(text, None)
}

/// Returns `text` in Unicode bidi display order with an explicit paragraph
/// base direction. Passing `None` performs the usual first-strong detection.
pub fn bidi_visual_order_with_base(text: &str, base: Option<Direction>) -> String {
    let base = base.map(|direction| match direction {
        Direction::Ltr => Level::ltr(),
        Direction::Rtl => Level::rtl(),
    });
    let bidi = BidiInfo::new(text, base);
    let mut visual = String::with_capacity(text.len());
    for paragraph in &bidi.paragraphs {
        visual.push_str(&bidi.reorder_line(paragraph, paragraph.range.clone()));
    }
    visual
}

/// Resolved base direction of the first paragraph, defaulting to LTR for
/// empty text. Consumers that serialize visual-order text can use this to
/// preserve the paragraph base explicitly at format boundaries.
pub fn bidi_base_direction(text: &str) -> Direction {
    BidiInfo::new(text, None)
        .paragraphs
        .first()
        .map_or(Direction::Ltr, |paragraph| {
            if paragraph.level.is_rtl() {
                Direction::Rtl
            } else {
                Direction::Ltr
            }
        })
}

fn split_script_runs(
    text: &str,
    range: Range<usize>,
    direction: Direction,
    out: &mut Vec<ItemizedRun>,
) {
    let slice = &text[range.clone()];
    let mut start = range.start;
    let mut active = Script::Common;
    for (offset, ch) in slice.char_indices() {
        let script = ch.script();
        if !matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
            if !matches!(active, Script::Common | Script::Inherited | Script::Unknown)
                && script != active
            {
                push_item(text, start..range.start + offset, direction, active, out);
                start = range.start + offset;
            }
            active = script;
        }
    }
    push_item(text, start..range.end, direction, active, out);
}

fn push_item(
    text: &str,
    range: Range<usize>,
    direction: Direction,
    script: Script,
    out: &mut Vec<ItemizedRun>,
) {
    if range.is_empty() {
        return;
    }
    let tag = *script
        .short_name()
        .as_bytes()
        .first_chunk::<4>()
        .unwrap_or(b"Zyyy");
    out.push(ItemizedRun {
        text: text[range.clone()].to_string(),
        range,
        direction,
        script: tag,
    });
}

/// Returns each uncovered scalar once, in first-occurrence order.
pub fn preflight_coverage(text: &str, faces: &[Face<'_>]) -> Vec<char> {
    let mut missing = Vec::new();
    for ch in text.chars() {
        if is_format_control(ch) || faces.iter().any(|face| face.covers(ch)) {
            continue;
        }
        if !missing.contains(&ch) {
            missing.push(ch);
        }
    }
    missing
}

/// Shapes one already-itemized run with a selected face.
pub fn shape_run(
    run: &ItemizedRun,
    face: &Face<'_>,
    face_index: usize,
) -> Result<ShapedRun, ShapeError> {
    shape_run_with_features(run, face, face_index, FeatureConfig::default())
}

/// Shapes one itemized run with explicit optional-feature overrides.
pub fn shape_run_with_features(
    run: &ItemizedRun,
    face: &Face<'_>,
    face_index: usize,
    feature_config: FeatureConfig,
) -> Result<ShapedRun, ShapeError> {
    let font = FontRef::from_index(face.bytes, face.index).map_err(|_| ShapeError::InvalidFont)?;
    let data = ShaperData::new(&font);
    let shaper = data.shaper(&font).build();
    let mut buffer = UnicodeBuffer::new();
    for (offset, ch) in run.text.char_indices() {
        buffer.add(ch, offset as u32);
    }
    buffer.set_direction(match run.direction {
        Direction::Ltr => HbDirection::LeftToRight,
        Direction::Rtl => HbDirection::RightToLeft,
    });
    buffer.set_script(
        HbScript::from_iso15924_tag(Tag::new(&run.script)).unwrap_or(harfrust::script::UNKNOWN),
    );
    let features = feature_config.harfrust_features();
    let shaped = shaper.shape(buffer, ShapeOptions::new().features(&features));
    let mut glyphs = shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, pos)| {
            let cluster = info.cluster as usize;
            if cluster > run.text.len() || !run.text.is_char_boundary(cluster) {
                return Err(ShapeError::InvalidCluster);
            }
            Ok(ShapedGlyph {
                glyph_id: info.glyph_id as u16,
                cluster,
                unicode_range: None,
                x_advance: pos.x_advance,
                y_advance: pos.y_advance,
                x_offset: pos.x_offset,
                y_offset: pos.y_offset,
                face_index,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
        return Err(ShapeError::MissingGlyphs(preflight_coverage(
            &run.text,
            std::slice::from_ref(face),
        )));
    }
    assign_unicode_ranges(&run.text, &mut glyphs);
    let advance = glyphs.iter().map(|glyph| glyph.x_advance).sum();
    Ok(ShapedRun {
        source: run.text.clone(),
        source_range: run.range.clone(),
        direction: run.direction,
        script: run.script,
        face_index,
        units_per_em: face.units_per_em,
        glyphs,
        advance,
    })
}

/// Itemizes, applies cluster-boundary fallback, and shapes one paragraph.
pub fn shape_text(text: &str, faces: &[Face<'_>]) -> Result<Vec<ShapedRun>, ShapeError> {
    shape_text_with_features(text, faces, FeatureConfig::default())
}

/// Itemizes, applies cluster-boundary fallback, and shapes one paragraph with
/// explicit optional-feature overrides.
pub fn shape_text_with_features(
    text: &str,
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
) -> Result<Vec<ShapedRun>, ShapeError> {
    let mut out = Vec::new();
    for item in itemize(text) {
        shape_item_with_features(&item, faces, feature_config, &mut out)?;
    }
    Ok(out)
}

fn shape_item_with_features(
    item: &ItemizedRun,
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
    out: &mut Vec<ShapedRun>,
) -> Result<(), ShapeError> {
    let mut pending: Option<(Range<usize>, usize)> = None;
    for cluster in approximate_clusters(&item.text) {
        if let Some(face_index) = find_covering_face(
            &item.text[cluster.clone()],
            item.direction,
            item.script,
            faces,
            feature_config,
        )? {
            if let Some((range, pending_face)) = &mut pending
                && *pending_face == face_index
            {
                range.end = cluster.end;
            } else {
                flush_normal_segment(item, pending.take(), faces, feature_config, out)?;
                pending = Some((cluster, face_index));
            }
            continue;
        }

        flush_normal_segment(item, pending.take(), faces, feature_config, out)?;
        let cluster_text = &item.text[cluster.clone()];
        if !is_emoji_sequence(cluster_text) {
            let item_missing = preflight_coverage(&item.text, faces);
            return Err(ShapeError::MissingGlyphs(if item_missing.is_empty() {
                missing_cluster_codepoints(cluster_text, faces)
            } else {
                item_missing
            }));
        }
        out.extend(shape_degraded_emoji_cluster(
            item,
            cluster,
            faces,
            feature_config,
        )?);
    }
    flush_normal_segment(item, pending, faces, feature_config, out)
}

fn find_covering_face(
    text: &str,
    direction: Direction,
    script: [u8; 4],
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
) -> Result<Option<usize>, ShapeError> {
    let verify_shaping = is_emoji_sequence(text);
    for (face_index, face) in faces.iter().enumerate() {
        if !text
            .chars()
            .all(|ch| is_format_control(ch) || face.covers(ch))
        {
            continue;
        }
        if !verify_shaping {
            return Ok(Some(face_index));
        }
        let probe = ItemizedRun {
            range: 0..text.len(),
            text: text.to_string(),
            direction,
            script,
        };
        match shape_run_with_features(&probe, face, face_index, feature_config) {
            Ok(_) => return Ok(Some(face_index)),
            Err(ShapeError::MissingGlyphs(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn flush_normal_segment(
    item: &ItemizedRun,
    pending: Option<(Range<usize>, usize)>,
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
    out: &mut Vec<ShapedRun>,
) -> Result<(), ShapeError> {
    let Some((local, face_index)) = pending else {
        return Ok(());
    };
    let run = ItemizedRun {
        range: item.range.start + local.start..item.range.start + local.end,
        text: item.text[local].to_string(),
        direction: item.direction,
        script: item.script,
    };
    out.push(shape_run_with_features(
        &run,
        &faces[face_index],
        face_index,
        feature_config,
    )?);
    Ok(())
}

fn shape_degraded_emoji_cluster(
    item: &ItemizedRun,
    local: Range<usize>,
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
) -> Result<Vec<ShapedRun>, ShapeError> {
    let original = &item.text[local.clone()];
    let filtered: String = original
        .chars()
        .filter(|ch| !is_emoji_variation_selector(*ch) && !is_skin_tone_modifier(*ch))
        .collect();
    let global = item.range.start + local.start..item.range.start + local.end;

    if !filtered.is_empty()
        && filtered != original
        && let Some(mut shaped) = shape_text_with_any_face(
            &filtered,
            item.direction,
            item.script,
            faces,
            feature_config,
        )?
    {
        mark_degraded_semantics(&mut shaped, original, global);
        return Ok(shaped);
    }

    if filtered.contains('\u{200D}') {
        let mut shaped = Vec::new();
        for component in filtered.split('\u{200D}').filter(|part| !part.is_empty()) {
            let Some(mut component_runs) = shape_text_with_any_face(
                component,
                item.direction,
                item.script,
                faces,
                feature_config,
            )?
            else {
                return Err(ShapeError::MissingGlyphs(missing_base_codepoints(
                    component, faces,
                )));
            };
            shaped.append(&mut component_runs);
        }
        if !shaped.is_empty() {
            mark_degraded_semantics(&mut shaped, original, global);
            return Ok(shaped);
        }
    }

    Err(ShapeError::MissingGlyphs(missing_base_codepoints(
        if filtered.is_empty() {
            original
        } else {
            &filtered
        },
        faces,
    )))
}

fn shape_text_with_any_face(
    text: &str,
    direction: Direction,
    script: [u8; 4],
    faces: &[Face<'_>],
    feature_config: FeatureConfig,
) -> Result<Option<Vec<ShapedRun>>, ShapeError> {
    let Some(face_index) = find_covering_face(text, direction, script, faces, feature_config)?
    else {
        return Ok(None);
    };
    let run = ItemizedRun {
        range: 0..text.len(),
        text: text.to_string(),
        direction,
        script,
    };
    Ok(Some(vec![shape_run_with_features(
        &run,
        &faces[face_index],
        face_index,
        feature_config,
    )?]))
}

fn mark_degraded_semantics(shaped: &mut [ShapedRun], original: &str, source_range: Range<usize>) {
    let mut owns_semantics = true;
    for run in shaped {
        run.source = original.to_string();
        run.source_range = source_range.clone();
        for glyph in &mut run.glyphs {
            glyph.cluster = 0;
            glyph.unicode_range = owns_semantics.then_some((0, original.len()));
            owns_semantics = false;
        }
    }
}

fn assign_unicode_ranges(source: &str, glyphs: &mut [ShapedGlyph]) {
    let mut starts: Vec<usize> = glyphs.iter().map(|glyph| glyph.cluster).collect();
    starts.sort_unstable();
    starts.dedup();
    let mut seen = HashSet::new();
    for glyph in glyphs {
        if !seen.insert(glyph.cluster) {
            glyph.unicode_range = None;
            continue;
        }
        let index = starts
            .binary_search(&glyph.cluster)
            .expect("glyph cluster came from starts");
        let end = starts.get(index + 1).copied().unwrap_or(source.len());
        glyph.unicode_range = Some((glyph.cluster, end));
    }
}

fn missing_cluster_codepoints(text: &str, faces: &[Face<'_>]) -> Vec<char> {
    let mut missing = preflight_coverage(text, faces);
    if missing.is_empty() {
        missing = unique_chars(text.chars().filter(|ch| !is_format_control(*ch)));
    }
    missing
}

fn missing_base_codepoints(text: &str, faces: &[Face<'_>]) -> Vec<char> {
    let bases = || {
        text.chars().filter(|ch| {
            !is_format_control(*ch)
                && *ch != '\u{200D}'
                && !is_emoji_variation_selector(*ch)
                && !is_skin_tone_modifier(*ch)
        })
    };
    let mut missing = unique_chars(bases().filter(|ch| !faces.iter().any(|face| face.covers(*ch))));
    if missing.is_empty() {
        missing = unique_chars(bases());
    }
    if missing.is_empty() {
        missing = unique_chars(text.chars().filter(|ch| !is_format_control(*ch)));
    }
    missing
}

fn unique_chars(chars: impl Iterator<Item = char>) -> Vec<char> {
    let mut unique = Vec::new();
    for ch in chars {
        if !unique.contains(&ch) {
            unique.push(ch);
        }
    }
    unique
}

fn approximate_clusters(text: &str) -> Vec<Range<usize>> {
    let mut starts = Vec::new();
    let mut join_next = false;
    for (offset, ch) in text.char_indices() {
        let continuation = ch.script() == Script::Inherited
            || matches!(ch as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
            || is_skin_tone_modifier(ch)
            || ch == '\u{200D}'
            || join_next;
        if !continuation {
            starts.push(offset);
        }
        join_next = ch == '\u{200D}';
    }
    starts
        .iter()
        .copied()
        .enumerate()
        .map(|(i, start)| start..starts.get(i + 1).copied().unwrap_or(text.len()))
        .collect()
}

fn is_emoji_sequence(text: &str) -> bool {
    text.chars().any(is_extended_pictographic_approx)
        && text.chars().any(|ch| {
            ch == '\u{200D}' || is_emoji_variation_selector(ch) || is_skin_tone_modifier(ch)
        })
}

fn is_emoji_variation_selector(ch: char) -> bool {
    ch == '\u{FE0F}'
}

fn is_skin_tone_modifier(ch: char) -> bool {
    matches!(ch as u32, 0x1F3FB..=0x1F3FF)
}

fn is_extended_pictographic_approx(ch: char) -> bool {
    matches!(ch as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
        | 0x2194..=0x21FF
        | 0x2300..=0x23FF
        | 0x24C2
        | 0x25AA..=0x27BF
        | 0x2934..=0x2935
        | 0x2B05..=0x2BFF
        | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x1F000..=0x1FAFF
    )
}

fn is_format_control(ch: char) -> bool {
    matches!(ch as u32, 0x200E..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}
