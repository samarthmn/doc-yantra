//! Candidate evaluation for Word-compatible Latin glyph advances.

use anyhow::{Context, Result};
use fidelity::evidence::sha256_hex;
use serde::Serialize;
use skrifa::{
    FontRef, GlyphId as SkrifaGlyphId, MetadataProvider,
    instance::{LocationRef, Size},
    outline::{Engine, HintingInstance, HintingOptions, SmoothMode, Target, pen::NullPen},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::RangeInclusive,
    path::Path,
};

use crate::{font_form_experiment, probe};
use lopdf::Document;

const CASE_PAGES: [(&str, u32, u32); 4] = [
    ("gdocs-anchors-toc", 5, 10),
    ("gdocs-tables-longdoc", 22, 44),
    ("gdocs-columns", 2, 3),
    ("korean", 5, 10),
];

struct Task2Partition;

fn task2_partition() -> Task2Partition {
    Task2Partition
}

impl Task2Partition {
    fn page_counts(&self, case: &str) -> (u32, u32) {
        CASE_PAGES
            .iter()
            .find(|(name, _, _)| *name == case)
            .map(|(_, discovery, total)| (*discovery, *total))
            .unwrap_or((1, 0))
    }

    fn discovery_pages(&self, case: &str) -> RangeInclusive<u32> {
        let (last, _) = self.page_counts(case);
        1..=last
    }

    fn held_out_pages(&self, case: &str) -> RangeInclusive<u32> {
        let (last_discovery, last) = self.page_counts(case);
        last_discovery.saturating_add(1)..=last
    }
}

fn quantize_accumulated(advances: &[f64]) -> Vec<f64> {
    let mut exact_end = 0.0;
    let mut quantized_end = 0.0;
    advances
        .iter()
        .map(|advance| {
            exact_end += advance;
            let next_end = exact_end.round();
            let quantized = next_end - quantized_end;
            quantized_end = next_end;
            quantized
        })
        .collect()
}

#[derive(Clone, Copy)]
struct FamilyAccuracy {
    samples: usize,
    within_one: usize,
    max_line_end_error_pt: f64,
}

fn passes_gate(families: &[FamilyAccuracy]) -> bool {
    families
        .iter()
        .filter(|family| {
            family.samples >= 100
                && family.within_one * 100 >= family.samples * 95
                && family.max_line_end_error_pt <= 0.10
        })
        .count()
        >= 3
}

fn is_latin_scalar(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    chars.next().is_none()
        && (ch.is_ascii() || matches!(ch as u32, 0x00A0..=0x024F | 0x1E00..=0x1EFF))
}

#[derive(Clone, Copy)]
enum PdfQuantization {
    None,
    PerGlyph,
    Accumulated,
}

#[derive(Clone, Copy)]
enum AdvanceSource {
    Design,
    PdfDesign,
    DeviceRound { dpi: u16 },
    DeviceSubpixelRound { dpi: u16 },
    Skrifa { dpi: u16, target: SkrifaTarget },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SkrifaTarget {
    MicrosoftGrayscale,
    FreeTypeGrayscale,
    Mono,
}

#[derive(Clone)]
struct Candidate {
    name: String,
    source: AdvanceSource,
    quantization: PdfQuantization,
}

fn candidates() -> Vec<Candidate> {
    let mut candidates = vec![
        Candidate {
            name: "design-exact".into(),
            source: AdvanceSource::Design,
            quantization: PdfQuantization::None,
        },
        Candidate {
            name: "design-pdf-per-glyph".into(),
            source: AdvanceSource::Design,
            quantization: PdfQuantization::PerGlyph,
        },
        Candidate {
            name: "design-pdf-accumulated".into(),
            source: AdvanceSource::Design,
            quantization: PdfQuantization::Accumulated,
        },
        Candidate {
            name: "design-quantized-before-shaping".into(),
            source: AdvanceSource::PdfDesign,
            quantization: PdfQuantization::None,
        },
        Candidate {
            name: "design-quantized-before-and-after-shaping".into(),
            source: AdvanceSource::PdfDesign,
            quantization: PdfQuantization::Accumulated,
        },
    ];
    for dpi in [72, 96, 120, 144] {
        let sources = [
            (
                format!("device-round-{dpi}dpi"),
                AdvanceSource::DeviceRound { dpi },
            ),
            (
                format!("device-subpixel64-round-{dpi}dpi"),
                AdvanceSource::DeviceSubpixelRound { dpi },
            ),
            (
                format!("skrifa-microsoft-grayscale-{dpi}dpi"),
                AdvanceSource::Skrifa {
                    dpi,
                    target: SkrifaTarget::MicrosoftGrayscale,
                },
            ),
            (
                format!("skrifa-freetype-grayscale-{dpi}dpi"),
                AdvanceSource::Skrifa {
                    dpi,
                    target: SkrifaTarget::FreeTypeGrayscale,
                },
            ),
            (
                format!("skrifa-mono-{dpi}dpi"),
                AdvanceSource::Skrifa {
                    dpi,
                    target: SkrifaTarget::Mono,
                },
            ),
        ];
        for (name, source) in sources {
            candidates.push(Candidate {
                name: format!("{name}-pdf-exact"),
                source,
                quantization: PdfQuantization::None,
            });
            candidates.push(Candidate {
                name: format!("{name}-pdf-per-glyph"),
                source,
                quantization: PdfQuantization::PerGlyph,
            });
            candidates.push(Candidate {
                name: format!("{name}-pdf-accumulated"),
                source,
                quantization: PdfQuantization::Accumulated,
            });
        }
    }
    candidates
}

fn replace_font_component(
    ours_placed: &[f64],
    raw_design: &[f64],
    candidate_font: &[f64],
    quantization: PdfQuantization,
) -> Vec<f64> {
    assert_eq!(ours_placed.len(), raw_design.len());
    assert_eq!(ours_placed.len(), candidate_font.len());
    let exact = ours_placed
        .iter()
        .zip(raw_design)
        .zip(candidate_font)
        .map(|((ours, design), candidate)| candidate + (ours - design))
        .collect::<Vec<_>>();
    match quantization {
        PdfQuantization::None => exact,
        PdfQuantization::PerGlyph => exact.into_iter().map(f64::round).collect(),
        PdfQuantization::Accumulated => quantize_accumulated(&exact),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AdvanceModelReport {
    discovery_partition: Vec<CasePartitionReport>,
    held_out_partition: Vec<CasePartitionReport>,
    discovery_evidence: EvidenceSummary,
    discovery_candidates: Vec<CandidateAccuracy>,
    selected_from_discovery: Option<String>,
    held_out_evidence: Option<EvidenceSummary>,
    held_out_accuracy: Option<CandidateAccuracy>,
    acceptance_gate_passed: bool,
}

#[derive(Debug, Serialize)]
struct CasePartitionReport {
    case: String,
    pages: Vec<u32>,
}

#[derive(Debug, Default, Serialize)]
struct EvidenceSummary {
    pages: usize,
    ours_text_records: usize,
    truth_text_records: usize,
    matched_text_records: usize,
    records_with_font_program: usize,
    latin_tj_samples: usize,
    font_programs: usize,
}

#[derive(Debug, Serialize)]
struct CandidateAccuracy {
    candidate: String,
    source: String,
    pdf_quantization: String,
    samples: usize,
    exact_tj: usize,
    within_one_tj: usize,
    exact_tj_pct: f64,
    within_one_tj_pct: f64,
    prediction_failures: usize,
    lines: usize,
    lines_within_0_10pt: usize,
    max_line_end_error_pt: f64,
    p95_line_end_error_pt: f64,
    qualifying_families: usize,
    gate_passed: bool,
    families: Vec<FamilyAccuracyReport>,
}

#[derive(Debug, Serialize)]
struct FamilyAccuracyReport {
    family: String,
    samples: usize,
    exact_tj: usize,
    within_one_tj: usize,
    exact_tj_pct: f64,
    within_one_tj_pct: f64,
    prediction_failures: usize,
    lines: usize,
    lines_within_0_10pt: usize,
    max_line_end_error_pt: f64,
    p95_line_end_error_pt: f64,
    styles: Vec<String>,
    sizes_pt: Vec<f64>,
    gate_passed: bool,
}

#[derive(Default)]
struct EvidenceSet {
    records: Vec<EvidenceRecord>,
    programs: Vec<Vec<u8>>,
    program_by_sha: BTreeMap<String, usize>,
    summary: EvidenceSummary,
}

struct EvidenceRecord {
    family: String,
    style: String,
    size_pt: f64,
    program: usize,
    glyphs: Vec<GlyphEvidence>,
}

struct GlyphEvidence {
    source_gid: u16,
    unicode: String,
    raw_design_text_units: f64,
    ours_placed_text_units: f64,
    truth_nominal_text_units: f64,
    truth_tj: f64,
}

#[derive(Default)]
struct AccuracyAccumulator {
    samples: usize,
    exact: usize,
    within_one: usize,
    prediction_failures: usize,
    line_errors: Vec<f64>,
    styles: BTreeSet<String>,
    sizes_milli_pt: BTreeSet<i64>,
}

pub(crate) fn run(ours_dir: &Path) -> Result<AdvanceModelReport> {
    let partition = task2_partition();
    let discovery_partition = partition_report(&partition, false);
    let held_out_partition = partition_report(&partition, true);
    let discovery = collect_evidence(ours_dir, &partition, false)?;
    let matrix = candidates();
    let mut discovery_candidates = matrix
        .iter()
        .map(|candidate| evaluate(candidate, &discovery))
        .collect::<Vec<_>>();
    let selected = discovery_candidates
        .iter()
        .enumerate()
        .filter(|(_, report)| report.samples != 0)
        .max_by(|(_, left), (_, right)| {
            left.within_one_tj_pct
                .total_cmp(&right.within_one_tj_pct)
                .then_with(|| {
                    right
                        .max_line_end_error_pt
                        .total_cmp(&left.max_line_end_error_pt)
                })
                .then_with(|| right.candidate.cmp(&left.candidate))
        })
        .map(|(index, _)| index);
    discovery_candidates.sort_by(|left, right| left.candidate.cmp(&right.candidate));

    let (selected_from_discovery, held_out_evidence, held_out_accuracy) =
        if let Some(selected) = selected {
            let candidate = &matrix[selected];
            // The validation set is not opened until the discovery-only selector
            // has frozen one deterministic candidate.
            let held_out = collect_evidence(ours_dir, &partition, true)?;
            let accuracy = evaluate(candidate, &held_out);
            (
                Some(candidate.name.clone()),
                Some(held_out.summary),
                Some(accuracy),
            )
        } else {
            (None, None, None)
        };
    let acceptance_gate_passed = held_out_accuracy
        .as_ref()
        .is_some_and(|accuracy| accuracy.gate_passed);

    Ok(AdvanceModelReport {
        discovery_partition,
        held_out_partition,
        discovery_evidence: discovery.summary,
        discovery_candidates,
        selected_from_discovery,
        held_out_evidence,
        held_out_accuracy,
        acceptance_gate_passed,
    })
}

fn partition_report(partition: &Task2Partition, held_out: bool) -> Vec<CasePartitionReport> {
    CASE_PAGES
        .iter()
        .map(|(case, _, _)| CasePartitionReport {
            case: (*case).to_string(),
            pages: if held_out {
                partition.held_out_pages(case).collect()
            } else {
                partition.discovery_pages(case).collect()
            },
        })
        .collect()
}

fn collect_evidence(
    ours_dir: &Path,
    partition: &Task2Partition,
    held_out: bool,
) -> Result<EvidenceSet> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/corpus/docx");
    let mut evidence = EvidenceSet::default();
    for (case, _, _) in CASE_PAGES {
        let ours_path = ours_dir.join(format!("{case}-ours.pdf"));
        let truth_path = corpus.join(case).join("ground-truth.pdf");
        let ours_pdf = std::fs::read(&ours_path)
            .with_context(|| format!("read candidate PDF {}", ours_path.display()))?;
        let truth_pdf = std::fs::read(&truth_path)
            .with_context(|| format!("read truth PDF {}", truth_path.display()))?;
        let ours_document = Document::load_mem(&ours_pdf)
            .with_context(|| format!("parse candidate PDF {}", ours_path.display()))?;
        let truth_document = Document::load_mem(&truth_pdf)
            .with_context(|| format!("parse truth PDF {}", truth_path.display()))?;
        let mut page_font_programs = BTreeMap::<(u32, String), Option<Vec<u8>>>::new();
        let pages = if held_out {
            partition.held_out_pages(case)
        } else {
            partition.discovery_pages(case)
        };
        for page in pages {
            evidence.summary.pages += 1;
            let (ours, _) = probe::extract_content_page_from_document(&ours_document, page)
                .with_context(|| format!("decode ours {case} page {page}"))?;
            let (truth, _) = probe::extract_content_page_from_document(&truth_document, page)
                .with_context(|| format!("decode truth {case} page {page}"))?;
            evidence.summary.ours_text_records += ours.text.len();
            evidence.summary.truth_text_records += truth.text.len();
            let pairs = match_records(&ours.text, &truth.text);
            evidence.summary.matched_text_records += pairs.len();
            for (ours_record, truth_record) in pairs {
                let program_key = (page, ours_record.resource_name.clone());
                if !page_font_programs.contains_key(&program_key) {
                    page_font_programs.insert(
                        program_key.clone(),
                        font_form_experiment::embedded_font_program_from_document(
                            &ours_document,
                            page,
                            &ours_record.resource_name,
                        )
                        .ok(),
                    );
                }
                let Some(program) = page_font_programs
                    .get(&program_key)
                    .and_then(Option::as_ref)
                else {
                    continue;
                };
                let face = match ttf_parser::Face::parse(program, 0) {
                    Ok(face) => face,
                    Err(_) => continue,
                };
                let mut glyphs = Vec::with_capacity(ours_record.glyphs.len());
                let mut valid = true;
                for (ours_glyph, truth_glyph) in ours_record.glyphs.iter().zip(&truth_record.glyphs)
                {
                    let Some(source_gid) = ours_glyph.source_gid else {
                        valid = false;
                        break;
                    };
                    let Some(raw_design) = face.glyph_hor_advance(ttf_parser::GlyphId(source_gid))
                    else {
                        valid = false;
                        break;
                    };
                    glyphs.push(GlyphEvidence {
                        source_gid,
                        unicode: ours_glyph.unicode.clone(),
                        raw_design_text_units: f64::from(raw_design)
                            / f64::from(face.units_per_em())
                            * 1_000.0,
                        ours_placed_text_units: ours_glyph.resulting_advance_text_units,
                        truth_nominal_text_units: truth_glyph.nominal_width_text_units,
                        truth_tj: truth_glyph.following_tj,
                    });
                }
                if !valid || !glyphs.iter().any(|glyph| is_latin_scalar(&glyph.unicode)) {
                    continue;
                }
                let hash = sha256_hex(program);
                let program_index = if let Some(index) = evidence.program_by_sha.get(&hash) {
                    *index
                } else {
                    let index = evidence.programs.len();
                    evidence.programs.push(program.clone());
                    evidence.program_by_sha.insert(hash, index);
                    index
                };
                evidence.summary.records_with_font_program += 1;
                evidence.summary.latin_tj_samples += glyphs
                    .iter()
                    .filter(|glyph| is_latin_scalar(&glyph.unicode))
                    .count();
                evidence.records.push(EvidenceRecord {
                    family: canonical_family(
                        truth_record.font_family.as_deref().unwrap_or_default(),
                    ),
                    style: truth_record.font_style.clone().unwrap_or_default(),
                    size_pt: truth_record.tf_pt,
                    program: program_index,
                    glyphs,
                });
            }
        }
    }
    evidence.summary.font_programs = evidence.programs.len();
    Ok(evidence)
}

fn match_records<'a>(
    ours: &'a [probe::TextShowRecord],
    truth: &'a [probe::TextShowRecord],
) -> Vec<(&'a probe::TextShowRecord, &'a probe::TextShowRecord)> {
    let mut used = BTreeSet::new();
    let mut pairs = Vec::new();
    for ours_record in ours {
        if ours_record.unicode.trim().is_empty() {
            continue;
        }
        let ours_glyphs = ours_record
            .glyphs
            .iter()
            .map(|glyph| glyph.unicode.as_str())
            .collect::<Vec<_>>();
        let best = truth
            .iter()
            .enumerate()
            .filter(|(index, truth_record)| {
                !used.contains(index)
                    && (truth_record.tf_pt - ours_record.tf_pt).abs() <= 0.01
                    && truth_record.unicode == ours_record.unicode
                    && truth_record.glyphs.len() == ours_record.glyphs.len()
                    && truth_record
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.unicode.as_str())
                        .eq(ours_glyphs.iter().copied())
            })
            .min_by(|(_, left), (_, right)| {
                origin_distance(ours_record, left).total_cmp(&origin_distance(ours_record, right))
            });
        if let Some((index, truth_record)) = best {
            used.insert(index);
            pairs.push((ours_record, truth_record));
        }
    }
    pairs
}

fn origin_distance(left: &probe::TextShowRecord, right: &probe::TextShowRecord) -> f64 {
    (left.origin[0] - right.origin[0]).hypot(left.origin[1] - right.origin[1])
}

fn evaluate(candidate: &Candidate, evidence: &EvidenceSet) -> CandidateAccuracy {
    let mut families = BTreeMap::<String, AccuracyAccumulator>::new();
    let metric_cache = candidate_metrics(candidate, evidence);
    for record in &evidence.records {
        let family = families.entry(record.family.clone()).or_default();
        family.styles.insert(record.style.clone());
        family
            .sizes_milli_pt
            .insert((record.size_pt * 1_000.0).round() as i64);
        let ours = record
            .glyphs
            .iter()
            .map(|glyph| glyph.ours_placed_text_units)
            .collect::<Vec<_>>();
        let design = record
            .glyphs
            .iter()
            .map(|glyph| glyph.raw_design_text_units)
            .collect::<Vec<_>>();
        let mut available = vec![true; record.glyphs.len()];
        let candidate_font = record
            .glyphs
            .iter()
            .enumerate()
            .map(|(index, glyph)| {
                if !is_latin_scalar(&glyph.unicode) {
                    return glyph.raw_design_text_units;
                }
                let key = (
                    record.program,
                    glyph.source_gid,
                    (record.size_pt * 1_000.0).round() as i64,
                );
                let advance = metric_cache.get(&key).copied().flatten();
                match advance {
                    Some(advance) => advance,
                    None => {
                        available[index] = false;
                        glyph.raw_design_text_units
                    }
                }
            })
            .collect::<Vec<_>>();
        let predicted =
            replace_font_component(&ours, &design, &candidate_font, candidate.quantization);
        let mut line_available = true;
        let mut target_count = 0;
        for (index, glyph) in record.glyphs.iter().enumerate() {
            if !is_latin_scalar(&glyph.unicode) {
                continue;
            }
            target_count += 1;
            family.samples += 1;
            if !available[index] {
                family.prediction_failures += 1;
                line_available = false;
                continue;
            }
            let predicted_tj = glyph.truth_nominal_text_units - predicted[index];
            let error = (predicted_tj - glyph.truth_tj).abs();
            family.exact += usize::from(error <= 0.01);
            family.within_one += usize::from(error <= 1.0 + 1e-9);
        }
        if target_count != 0 {
            let error_pt = if line_available {
                let predicted_end = predicted.iter().sum::<f64>();
                let truth_end = record
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.truth_nominal_text_units - glyph.truth_tj)
                    .sum::<f64>();
                (predicted_end - truth_end).abs() / 1_000.0 * record.size_pt
            } else {
                1_000_000.0
            };
            family.line_errors.push(error_pt);
        }
    }

    let mut family_reports = families
        .into_iter()
        .map(|(family, accuracy)| family_report(family, accuracy))
        .collect::<Vec<_>>();
    family_reports.sort_by(|left, right| left.family.cmp(&right.family));
    let samples = family_reports.iter().map(|family| family.samples).sum();
    let exact = family_reports.iter().map(|family| family.exact_tj).sum();
    let within_one = family_reports
        .iter()
        .map(|family| family.within_one_tj)
        .sum();
    let failures = family_reports
        .iter()
        .map(|family| family.prediction_failures)
        .sum();
    let lines = family_reports.iter().map(|family| family.lines).sum();
    let lines_within = family_reports
        .iter()
        .map(|family| family.lines_within_0_10pt)
        .sum();
    let max_line = family_reports
        .iter()
        .map(|family| family.max_line_end_error_pt)
        .fold(0.0, f64::max);
    let p95_line = family_reports
        .iter()
        .map(|family| family.p95_line_end_error_pt)
        .fold(0.0, f64::max);
    let qualifying_families = family_reports
        .iter()
        .filter(|family| family.gate_passed)
        .count();
    let gate_inputs = family_reports
        .iter()
        .map(|family| FamilyAccuracy {
            samples: family.samples,
            within_one: family.within_one_tj,
            max_line_end_error_pt: family.max_line_end_error_pt,
        })
        .collect::<Vec<_>>();
    CandidateAccuracy {
        candidate: candidate.name.clone(),
        source: source_label(candidate.source),
        pdf_quantization: quantization_label(candidate.quantization).into(),
        samples,
        exact_tj: exact,
        within_one_tj: within_one,
        exact_tj_pct: percentage(exact, samples),
        within_one_tj_pct: percentage(within_one, samples),
        prediction_failures: failures,
        lines,
        lines_within_0_10pt: lines_within,
        max_line_end_error_pt: max_line,
        p95_line_end_error_pt: p95_line,
        qualifying_families,
        gate_passed: passes_gate(&gate_inputs),
        families: family_reports,
    }
}

fn candidate_metrics(
    candidate: &Candidate,
    evidence: &EvidenceSet,
) -> BTreeMap<(usize, u16, i64), Option<f64>> {
    let mut requests = BTreeMap::<(usize, i64), BTreeMap<u16, f64>>::new();
    for record in &evidence.records {
        let size = (record.size_pt * 1_000.0).round() as i64;
        for glyph in &record.glyphs {
            if is_latin_scalar(&glyph.unicode) {
                requests
                    .entry((record.program, size))
                    .or_default()
                    .entry(glyph.source_gid)
                    .or_insert(glyph.raw_design_text_units);
            }
        }
    }
    let mut metrics = BTreeMap::new();
    for ((program, size_milli_pt), glyphs) in requests {
        let size_pt = size_milli_pt as f64 / 1_000.0;
        match candidate.source {
            AdvanceSource::Design
            | AdvanceSource::PdfDesign
            | AdvanceSource::DeviceRound { .. }
            | AdvanceSource::DeviceSubpixelRound { .. } => {
                for (gid, raw_design) in glyphs {
                    metrics.insert(
                        (program, gid, size_milli_pt),
                        candidate_advance(
                            candidate.source,
                            &evidence.programs[program],
                            gid,
                            size_pt,
                            raw_design,
                        ),
                    );
                }
            }
            AdvanceSource::Skrifa { dpi, target } => {
                let advances = skrifa_advances(
                    &evidence.programs[program],
                    glyphs.keys().copied(),
                    size_pt,
                    dpi,
                    target,
                );
                for gid in glyphs.keys() {
                    metrics.insert(
                        (program, *gid, size_milli_pt),
                        advances.get(gid).copied().flatten(),
                    );
                }
            }
        }
    }
    metrics
}

fn family_report(family: String, mut accuracy: AccuracyAccumulator) -> FamilyAccuracyReport {
    accuracy.line_errors.sort_by(f64::total_cmp);
    let max_line = accuracy.line_errors.last().copied().unwrap_or(0.0);
    let p95_line = percentile(&accuracy.line_errors, 0.95);
    let line_count = accuracy.line_errors.len();
    let lines_within = accuracy
        .line_errors
        .iter()
        .filter(|error| **error <= 0.10 + 1e-12)
        .count();
    let gate_passed = accuracy.samples >= 100
        && accuracy.within_one * 100 >= accuracy.samples * 95
        && max_line <= 0.10;
    FamilyAccuracyReport {
        family,
        samples: accuracy.samples,
        exact_tj: accuracy.exact,
        within_one_tj: accuracy.within_one,
        exact_tj_pct: percentage(accuracy.exact, accuracy.samples),
        within_one_tj_pct: percentage(accuracy.within_one, accuracy.samples),
        prediction_failures: accuracy.prediction_failures,
        lines: line_count,
        lines_within_0_10pt: lines_within,
        max_line_end_error_pt: max_line,
        p95_line_end_error_pt: p95_line,
        styles: accuracy.styles.into_iter().collect(),
        sizes_pt: accuracy
            .sizes_milli_pt
            .into_iter()
            .map(|size| size as f64 / 1_000.0)
            .collect(),
        gate_passed,
    }
}

fn candidate_advance(
    source: AdvanceSource,
    program: &[u8],
    gid: u16,
    size_pt: f64,
    raw_design_text_units: f64,
) -> Option<f64> {
    match source {
        AdvanceSource::Design => Some(raw_design_text_units),
        AdvanceSource::PdfDesign => Some(raw_design_text_units.round()),
        AdvanceSource::DeviceRound { dpi } => {
            let ppem = size_pt * f64::from(dpi) / 72.0;
            (ppem > 0.0).then(|| (raw_design_text_units / 1_000.0 * ppem).round() / ppem * 1_000.0)
        }
        AdvanceSource::DeviceSubpixelRound { dpi } => {
            let ppem = size_pt * f64::from(dpi) / 72.0;
            (ppem > 0.0).then(|| {
                (raw_design_text_units / 1_000.0 * ppem * 64.0).round() / (ppem * 64.0) * 1_000.0
            })
        }
        AdvanceSource::Skrifa { dpi, target } => skrifa_advance(program, gid, size_pt, dpi, target),
    }
}

fn skrifa_advance(
    program: &[u8],
    gid: u16,
    size_pt: f64,
    dpi: u16,
    target: SkrifaTarget,
) -> Option<f64> {
    skrifa_advances(program, [gid], size_pt, dpi, target)
        .remove(&gid)
        .flatten()
}

fn skrifa_advances(
    program: &[u8],
    gids: impl IntoIterator<Item = u16>,
    size_pt: f64,
    dpi: u16,
    target: SkrifaTarget,
) -> BTreeMap<u16, Option<f64>> {
    let gids = gids.into_iter().collect::<Vec<_>>();
    let ppem = size_pt * f64::from(dpi) / 72.0;
    if ppem <= 0.0 {
        return gids.into_iter().map(|gid| (gid, None)).collect();
    }
    let Some(font) = FontRef::new(program).ok() else {
        return gids.into_iter().map(|gid| (gid, None)).collect();
    };
    let outlines = font.outline_glyphs();
    let options = HintingOptions {
        engine: Engine::Interpreter,
        target: match target {
            SkrifaTarget::MicrosoftGrayscale => Target::Smooth {
                mode: SmoothMode::Normal,
                symmetric_rendering: false,
                preserve_linear_metrics: false,
            },
            SkrifaTarget::FreeTypeGrayscale => Target::Smooth {
                mode: SmoothMode::Normal,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            },
            SkrifaTarget::Mono => Target::Mono,
        },
    };
    let Some(hinter) = HintingInstance::new(
        &outlines,
        Size::new(ppem as f32),
        LocationRef::default(),
        options,
    )
    .ok() else {
        return gids.into_iter().map(|gid| (gid, None)).collect();
    };
    gids.into_iter()
        .map(|gid| {
            let advance = outlines
                .get(SkrifaGlyphId::new(u32::from(gid)))
                .and_then(|glyph| glyph.draw(&hinter, &mut NullPen).ok())
                .and_then(|adjusted| adjusted.advance_width)
                .map(|advance| f64::from(advance) / ppem * 1_000.0);
            (gid, advance)
        })
        .collect()
}

fn canonical_family(name: &str) -> String {
    let mut name = name.trim_start_matches('/');
    if name.as_bytes().get(6) == Some(&b'+')
        && name.as_bytes()[..6].iter().all(u8::is_ascii_uppercase)
    {
        name = &name[7..];
    }
    for suffix in [
        "-BoldItalicMT",
        "-BoldMT",
        "-ItalicMT",
        "-Regular",
        "PSMT",
        "MT",
    ] {
        if let Some(family) = name.strip_suffix(suffix) {
            return family.to_string();
        }
    }
    name.to_string()
}

fn source_label(source: AdvanceSource) -> String {
    match source {
        AdvanceSource::Design => "design".into(),
        AdvanceSource::PdfDesign => "design-rounded-to-pdf-text-unit".into(),
        AdvanceSource::DeviceRound { dpi } => format!("design-device-rounded-{dpi}dpi"),
        AdvanceSource::DeviceSubpixelRound { dpi } => {
            format!("design-device-subpixel64-rounded-{dpi}dpi")
        }
        AdvanceSource::Skrifa { dpi, target } => {
            format!("skrifa-interpreter-{target:?}-{dpi}dpi")
        }
    }
}

fn quantization_label(quantization: PdfQuantization) -> &'static str {
    match quantization {
        PdfQuantization::None => "none",
        PdfQuantization::PerGlyph => "per-glyph",
        PdfQuantization::Accumulated => "accumulated",
    }
}

fn percentage(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task2_partition_keeps_validation_pages_out_of_discovery() {
        let partition = task2_partition();
        assert_eq!(partition.discovery_pages("gdocs-anchors-toc"), 1..=5);
        assert_eq!(partition.discovery_pages("gdocs-tables-longdoc"), 1..=22);
        assert_eq!(partition.discovery_pages("gdocs-columns"), 1..=2);
        assert_eq!(partition.discovery_pages("korean"), 1..=5);
        assert_eq!(partition.held_out_pages("gdocs-anchors-toc"), 6..=10);
        assert_eq!(partition.held_out_pages("gdocs-tables-longdoc"), 23..=44);
        assert_eq!(partition.held_out_pages("gdocs-columns"), 3..=3);
        assert_eq!(partition.held_out_pages("korean"), 6..=10);
    }

    #[test]
    fn accumulated_pdf_rounding_emits_context_compensation() {
        assert_eq!(
            quantize_accumulated(&[500.4, 500.4, 500.4]),
            vec![500.0, 501.0, 500.0]
        );
    }

    #[test]
    fn task2_gate_requires_accuracy_and_line_ends_for_three_families() {
        let passing = FamilyAccuracy {
            samples: 100,
            within_one: 95,
            max_line_end_error_pt: 0.10,
        };
        let inaccurate = FamilyAccuracy {
            samples: 100,
            within_one: 94,
            max_line_end_error_pt: 0.01,
        };
        let drifting = FamilyAccuracy {
            samples: 100,
            within_one: 100,
            max_line_end_error_pt: 0.100_001,
        };
        assert!(passes_gate(&[passing; 3]));
        assert!(!passes_gate(&[passing, passing, inaccurate]));
        assert!(!passes_gate(&[passing, passing, drifting]));
    }

    #[test]
    fn latin_classifier_excludes_cjk_and_combining_clusters() {
        assert!(is_latin_scalar("A"));
        assert!(is_latin_scalar("é"));
        assert!(!is_latin_scalar("한"));
        assert!(!is_latin_scalar("e\u{301}"));
    }

    #[test]
    fn candidate_matrix_covers_required_device_and_interpreter_modes() {
        let names = candidates()
            .into_iter()
            .map(|candidate| candidate.name)
            .collect::<Vec<_>>();
        for dpi in [72, 96, 120, 144] {
            assert!(names.contains(&format!("device-round-{dpi}dpi-pdf-accumulated")));
            assert!(names.contains(&format!(
                "skrifa-microsoft-grayscale-{dpi}dpi-pdf-accumulated"
            )));
            assert!(names.contains(&format!(
                "skrifa-freetype-grayscale-{dpi}dpi-pdf-accumulated"
            )));
            assert!(names.contains(&format!("skrifa-mono-{dpi}dpi-pdf-accumulated")));
        }
    }

    #[test]
    fn font_candidate_replaces_design_width_but_preserves_layout_delta() {
        let ours_placed = [510.25, 480.25, 510.25];
        let raw_design = [500.0, 500.0, 500.0];
        let candidate_font = [501.4, 498.4, 501.4];
        assert_eq!(
            replace_font_component(
                &ours_placed,
                &raw_design,
                &candidate_font,
                PdfQuantization::Accumulated,
            ),
            vec![512.0, 478.0, 512.0]
        );
    }
}
