//! Controlled PDF font-form raster experiment.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use fidelity::evidence::sha256_hex;
use image::{DynamicImage, ImageFormat};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat, dictionary};
use office_core::fonts::{FontDb, subset_for_pdf};
use serde::{Deserialize, Serialize};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::{font_raster_probe, probe, raster};

const PHASES_PT: [f64; 4] = [-0.125, 0.0, 0.125, 0.25];

#[derive(Debug, Clone, Copy, PartialEq)]
struct SampleSpec {
    case: &'static str,
    page: u32,
    truth_family: &'static str,
    size_pt: f64,
}

#[derive(Debug, Clone, Copy)]
struct FamilySpec {
    key: &'static str,
    family: &'static str,
    dfonts_file: Option<&'static str>,
    face_index: u32,
    samples: &'static [SampleSpec],
}

const UBUNTU_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "gdocs-anchors-toc",
        page: 1,
        truth_family: "Ubuntu-Regular",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "gdocs-anchors-toc",
        page: 4,
        truth_family: "Ubuntu-Regular",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "gdocs-anchors-toc",
        page: 10,
        truth_family: "Ubuntu-Regular",
        size_pt: 12.0,
    },
];

const CALIBRI_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "gdocs-columns",
        page: 1,
        truth_family: "Calibri",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "gdocs-columns",
        page: 2,
        truth_family: "Calibri",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "gdocs-columns",
        page: 3,
        truth_family: "Calibri",
        size_pt: 11.0,
    },
];

const ARIAL_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "korean",
        page: 1,
        truth_family: "ArialMT",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "korean",
        page: 2,
        truth_family: "ArialMT",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "korean",
        page: 7,
        truth_family: "ArialMT",
        size_pt: 11.0,
    },
];

const SIMSUN_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "chinese",
        page: 1,
        truth_family: "SimSun",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "chinese",
        page: 2,
        truth_family: "SimSun",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "chinese",
        page: 5,
        truth_family: "SimSun",
        size_pt: 12.0,
    },
];

const MS_GOTHIC_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "japanese",
        page: 1,
        truth_family: "MS-Gothic",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "japanese",
        page: 2,
        truth_family: "MS-Gothic",
        size_pt: 12.0,
    },
    SampleSpec {
        case: "japanese",
        page: 5,
        truth_family: "MS-Gothic",
        size_pt: 12.0,
    },
];

const BATANG_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "korean",
        page: 1,
        truth_family: "Batang",
        size_pt: 14.0,
    },
    SampleSpec {
        case: "korean",
        page: 2,
        truth_family: "Batang",
        size_pt: 14.0,
    },
    SampleSpec {
        case: "korean",
        page: 3,
        truth_family: "Batang",
        size_pt: 14.0,
    },
];

const CAMBRIA_MATH_SAMPLES: &[SampleSpec] = &[
    SampleSpec {
        case: "chinese",
        page: 2,
        truth_family: "CambriaMath",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "japanese",
        page: 3,
        truth_family: "CambriaMath",
        size_pt: 11.0,
    },
    SampleSpec {
        case: "korean",
        page: 3,
        truth_family: "CambriaMath",
        size_pt: 11.0,
    },
];

const FAMILIES: &[FamilySpec] = &[
    FamilySpec {
        key: "ubuntu",
        family: "Ubuntu",
        dfonts_file: None,
        face_index: 0,
        samples: UBUNTU_SAMPLES,
    },
    FamilySpec {
        key: "calibri",
        family: "Calibri",
        dfonts_file: Some("Calibri.ttf"),
        face_index: 0,
        samples: CALIBRI_SAMPLES,
    },
    FamilySpec {
        key: "arial",
        family: "Arial",
        dfonts_file: Some("arial.ttf"),
        face_index: 0,
        samples: ARIAL_SAMPLES,
    },
    FamilySpec {
        key: "simsun",
        family: "SimSun-class",
        dfonts_file: Some("Simsun.ttc"),
        face_index: 0,
        samples: SIMSUN_SAMPLES,
    },
    FamilySpec {
        key: "ms-gothic",
        family: "MS Gothic-class",
        dfonts_file: Some("msgothic.ttc"),
        face_index: 0,
        samples: MS_GOTHIC_SAMPLES,
    },
    FamilySpec {
        key: "batang",
        family: "Batang-class",
        dfonts_file: Some("batang.ttc"),
        face_index: 0,
        samples: BATANG_SAMPLES,
    },
    FamilySpec {
        key: "cambria-math",
        family: "Cambria Math",
        dfonts_file: Some("Cambria.ttc"),
        face_index: 1,
        samples: CAMBRIA_MATH_SAMPLES,
    },
];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProgramStats {
    pub bytes: usize,
    pub glyph_slots: u16,
    pub tracked_tables: Vec<String>,
    pub glyph_instruction_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct FontToolsReport {
    fonttools_version: String,
    peak_rss_bytes: u64,
    font: ProgramStats,
}

#[derive(Debug, Serialize)]
pub struct ExperimentReport {
    pub scorer: String,
    pub raster_scale: f64,
    pub phases_pt: Vec<[f64; 2]>,
    pub fonttools_version: String,
    pub families: Vec<FamilyReport>,
}

#[derive(Debug, Serialize)]
pub struct FamilyReport {
    pub family: String,
    pub legal_source: PathBuf,
    pub source_sha256: String,
    pub face_index: u32,
    pub full_program: ProgramStats,
    pub full_extract_peak_rss_bytes: u64,
    pub samples: Vec<SampleReport>,
    pub aggregate: Vec<AggregateReport>,
}

#[derive(Debug, Serialize)]
pub struct SampleReport {
    pub case: String,
    pub page: u32,
    pub truth_family: String,
    pub truth_font_subtype: Option<String>,
    pub truth_font_encoding: Option<String>,
    pub size_pt: f64,
    pub text: String,
    pub glyphs: usize,
    pub crop_px: [u32; 4],
    pub truth_page_px: [u32; 2],
    pub target_lines_on_page: usize,
    pub source_truth_width_matches: usize,
    pub source_truth_outline_matches: usize,
    pub source_truth_comparable_glyphs: usize,
    pub truth_program: ProgramStats,
    pub variants: Vec<VariantReport>,
}

#[derive(Debug, Serialize)]
pub struct VariantReport {
    pub form: String,
    pub applicable: bool,
    pub reason: Option<String>,
    pub program: Option<ProgramStats>,
    pub pdf_bytes: Option<usize>,
    pub modeled_live_bytes: Option<usize>,
    pub preparation_peak_rss_bytes: Option<u64>,
    pub extraction_parity: Option<bool>,
    pub max_origin_error_pt: Option<f64>,
    pub exact_phase: Option<PhaseScore>,
    pub best_phase: Option<PhaseScore>,
    pub phase_mean_ssim: Option<f64>,
    pub phase_mean_ssim_unweighted: Option<f64>,
    pub scores: Vec<PhaseScore>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseScore {
    pub x_pt: f64,
    pub y_pt: f64,
    pub ssim: f64,
    pub ssim_unweighted: f64,
    pub diff_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct AggregateReport {
    pub form: String,
    pub applicable_samples: usize,
    pub exact_mean_ssim: Option<f64>,
    pub exact_delta_from_compact: Option<f64>,
    pub exact_mean_ssim_unweighted: Option<f64>,
    pub exact_delta_from_compact_unweighted: Option<f64>,
    pub best_phase_mean_ssim: Option<f64>,
    pub best_phase_delta_from_compact: Option<f64>,
    pub best_phase_mean_ssim_unweighted: Option<f64>,
    pub best_phase_delta_from_compact_unweighted: Option<f64>,
    pub mean_program_bytes: Option<f64>,
    pub mean_pdf_bytes: Option<f64>,
    pub estimated_page_delta_unweighted_area_model: Option<f64>,
}

#[derive(Debug, Clone)]
struct PlacedGlyph {
    character: char,
    source_gid: u16,
    x_pt: f64,
    y_pt: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontForm {
    CompactType0,
    CompactHintedType0Control,
    SparseType0,
    FullType0,
    SimpleWinAnsi,
    OutlinePaths,
}

impl FontForm {
    const ALL: [FontForm; 6] = [
        FontForm::CompactType0,
        FontForm::CompactHintedType0Control,
        FontForm::SparseType0,
        FontForm::FullType0,
        FontForm::SimpleWinAnsi,
        FontForm::OutlinePaths,
    ];

    fn label(self) -> &'static str {
        match self {
            FontForm::CompactType0 => "compact_type0",
            FontForm::CompactHintedType0Control => "compact_hinted_type0_control",
            FontForm::SparseType0 => "sparse_original_gid_type0",
            FontForm::FullType0 => "full_font_type0",
            FontForm::SimpleWinAnsi => "simple_truetype_winansi",
            FontForm::OutlinePaths => "outline_paths",
        }
    }
}

struct CandidateProgram<'a> {
    data: Option<&'a [u8]>,
    remap: BTreeMap<u16, u16>,
    stats: Option<ProgramStats>,
    peak_rss_bytes: Option<u64>,
}

pub fn run(
    output_dir: &Path,
    ubuntu_font: &Path,
    dfonts_dir: &Path,
    python: &Path,
) -> Result<ExperimentReport> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("create experiment directory {}", output_dir.display()))?;
    let phases = phases();
    let mut families = Vec::new();
    let mut fonttools_version = None;
    for family in FAMILIES {
        let source = family
            .dfonts_file
            .map_or_else(|| ubuntu_font.to_path_buf(), |file| dfonts_dir.join(file));
        if !source.is_file() {
            bail!("legal font source is missing: {}", source.display());
        }
        let family_dir = output_dir.join(family.key);
        std::fs::create_dir_all(&family_dir)?;
        let full_path = family_dir.join("full.ttf");
        let extracted = run_fonttools(
            python,
            &[
                "extract".into(),
                "--source".into(),
                source.as_os_str().into(),
                "--face-index".into(),
                family.face_index.to_string().into(),
                "--output".into(),
                full_path.as_os_str().into(),
            ],
        )?;
        fonttools_version.get_or_insert(extracted.fonttools_version.clone());
        let full_bytes = std::fs::read(&full_path)?;
        let source_sha256 = sha256_hex(&std::fs::read(&source)?);
        let mut samples = Vec::new();
        for sample in family.samples {
            samples.push(run_sample(
                family,
                sample,
                &family_dir,
                &full_bytes,
                &extracted.font,
                extracted.peak_rss_bytes,
                python,
                &phases,
            )?);
        }
        let aggregate = aggregate(&samples);
        families.push(FamilyReport {
            family: family.family.to_string(),
            legal_source: source,
            source_sha256,
            face_index: family.face_index,
            full_program: extracted.font,
            full_extract_peak_rss_bytes: extracted.peak_rss_bytes,
            samples,
            aggregate,
        });
    }
    Ok(ExperimentReport {
        scorer: "unchanged fidelity::ssim::score_pages (8x8 windows)".to_string(),
        raster_scale: 2.0,
        phases_pt: phases,
        fonttools_version: fonttools_version.unwrap_or_default(),
        families,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "one sample needs its fixed evidence and prepared source face"
)]
fn run_sample(
    family: &FamilySpec,
    sample: &SampleSpec,
    family_dir: &Path,
    full_bytes: &[u8],
    full_stats: &ProgramStats,
    full_peak_rss: u64,
    python: &Path,
    phases: &[[f64; 2]],
) -> Result<SampleReport> {
    let truth_path = corpus_root()
        .join("docx")
        .join(sample.case)
        .join("ground-truth.pdf");
    let truth_pdf = std::fs::read(&truth_path)
        .with_context(|| format!("read truth PDF {}", truth_path.display()))?;
    let truth_content = probe::extract_content_page(&truth_pdf, sample.page)?;
    let face = Face::parse(full_bytes, 0).context("parse extracted legal font face")?;
    let record = select_record(&truth_content, sample, &face)?;
    let placed = placed_glyphs(record, &face)?;
    let used: BTreeSet<u16> = placed.iter().map(|glyph| glyph.source_gid).collect();
    let page_size = pdf_page_size(&truth_pdf, sample.page)?;
    let truth_render = raster(&truth_pdf, sample.page - 1)?;
    let crop = glyph_crop(&placed, &face, record.tf_pt, page_size, &truth_render)?;
    let target_lines_on_page = target_line_count(&truth_content, sample);

    let truth_program = embedded_font_program(&truth_pdf, sample.page, &record.resource_name)?;
    let truth_program_path = family_dir.join(format!(
        "{}-p{}-truth-inspection.ttf",
        sample.case, sample.page
    ));
    std::fs::write(&truth_program_path, &truth_program)?;
    let truth_stats = run_fonttools(
        python,
        &[
            "stats".into(),
            "--source".into(),
            truth_program_path.as_os_str().into(),
        ],
    )?
    .font;
    let truth_face = Face::parse(&truth_program, 0).context("parse truth font program")?;
    let (width_matches, outline_matches, comparable) =
        source_truth_matches(record, &placed, &face, &truth_face);

    let sample_stem = format!("{}-p{}", sample.case, sample.page);
    let sparse_path = family_dir.join(format!("{sample_stem}-sparse.ttf"));
    let gids = used
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let sparse_tool = run_fonttools(
        python,
        &[
            "sparse".into(),
            "--source".into(),
            family_dir.join("full.ttf").as_os_str().into(),
            "--gids".into(),
            gids.into(),
            "--output".into(),
            sparse_path.as_os_str().into(),
        ],
    )?;
    let sparse_bytes = std::fs::read(&sparse_path)?;

    let (compact_hinted_bytes, compact_remap) = compact_subset(full_bytes, &placed)?;
    let compact_hinted_path = family_dir.join(format!("{sample_stem}-compact-hinted.ttf"));
    std::fs::write(&compact_hinted_path, &compact_hinted_bytes)?;
    let compact_hinted_tool = run_fonttools(
        python,
        &[
            "stats".into(),
            "--source".into(),
            compact_hinted_path.as_os_str().into(),
        ],
    )?;
    let compact_hinted_stats = compact_hinted_tool.font.clone();
    let compact_path = family_dir.join(format!("{sample_stem}-compact-dehinted.ttf"));
    let compact_tool = run_fonttools(
        python,
        &[
            "dehint".into(),
            "--source".into(),
            compact_hinted_path.as_os_str().into(),
            "--output".into(),
            compact_path.as_os_str().into(),
        ],
    )?;
    let compact_bytes = std::fs::read(&compact_path)?;

    let identity: BTreeMap<u16, u16> = used.iter().map(|gid| (*gid, *gid)).collect();
    let mut variants = Vec::new();
    for form in FontForm::ALL {
        if form == FontForm::SimpleWinAnsi
            && placed
                .iter()
                .any(|glyph| winansi_encode(glyph.character).is_none())
        {
            variants.push(VariantReport {
                form: form.label().to_string(),
                applicable: false,
                reason: Some("line is not wholly representable in WinAnsi/CP1252".to_string()),
                program: None,
                pdf_bytes: None,
                modeled_live_bytes: None,
                preparation_peak_rss_bytes: None,
                extraction_parity: None,
                max_origin_error_pt: None,
                exact_phase: None,
                best_phase: None,
                phase_mean_ssim: None,
                phase_mean_ssim_unweighted: None,
                scores: Vec::new(),
            });
            continue;
        }
        let program = match form {
            FontForm::CompactType0 => CandidateProgram {
                data: Some(&compact_bytes),
                remap: compact_remap.clone(),
                stats: Some(compact_tool.font.clone()),
                peak_rss_bytes: Some(compact_tool.peak_rss_bytes),
            },
            FontForm::CompactHintedType0Control => CandidateProgram {
                data: Some(&compact_hinted_bytes),
                remap: compact_remap.clone(),
                stats: Some(compact_hinted_stats.clone()),
                peak_rss_bytes: Some(compact_hinted_tool.peak_rss_bytes),
            },
            FontForm::SparseType0 => CandidateProgram {
                data: Some(&sparse_bytes),
                remap: identity.clone(),
                stats: Some(sparse_tool.font.clone()),
                peak_rss_bytes: Some(sparse_tool.peak_rss_bytes),
            },
            FontForm::FullType0 | FontForm::SimpleWinAnsi => CandidateProgram {
                data: Some(full_bytes),
                remap: identity.clone(),
                stats: Some(full_stats.clone()),
                peak_rss_bytes: Some(full_peak_rss),
            },
            FontForm::OutlinePaths => CandidateProgram {
                data: None,
                remap: identity.clone(),
                stats: None,
                peak_rss_bytes: None,
            },
        };
        let pdf = candidate_pdf(
            form,
            &program,
            &placed,
            &face,
            record.tf_pt,
            page_size,
            phases,
        )?;
        let pdf_path = family_dir.join(format!("{sample_stem}-{}.pdf", form.label()));
        std::fs::write(&pdf_path, &pdf)?;
        let exact_index = phases
            .iter()
            .position(|phase| phase == &[0.0, 0.0])
            .context("phase grid must include exact origin")?;
        let mut scores = Vec::new();
        let mut exact_render = None;
        for (index, [x_pt, y_pt]) in phases.iter().copied().enumerate() {
            let rendered = raster(&pdf, index as u32)?;
            let score = font_raster_probe::score_crop(&rendered, &truth_render, Some(crop))?;
            if index == exact_index {
                exact_render = Some(rendered.clone());
            }
            scores.push(PhaseScore {
                x_pt,
                y_pt,
                ssim: score.ssim,
                ssim_unweighted: score.ssim_unweighted,
                diff_pct: score.diff_pct,
            });
        }
        let exact_phase = scores[exact_index].clone();
        let best_phase = scores
            .iter()
            .max_by(|left, right| left.ssim.total_cmp(&right.ssim))
            .cloned()
            .context("phase sweep produced no scores")?;
        let phase_mean_ssim =
            scores.iter().map(|score| score.ssim).sum::<f64>() / scores.len() as f64;
        let phase_mean = scores
            .iter()
            .map(|score| score.ssim_unweighted)
            .sum::<f64>()
            / scores.len() as f64;
        let (extraction_parity, max_origin_error_pt) = if form == FontForm::OutlinePaths {
            (Some(false), Some(0.0))
        } else {
            candidate_text_checks(&pdf, exact_index as u32 + 1, record, &placed)?
        };
        if sample == family.samples.first().expect("every family has samples") {
            let visual_dir = family_dir.join("visual");
            std::fs::create_dir_all(&visual_dir)?;
            save_crop(
                exact_render
                    .as_ref()
                    .context("missing exact phase render")?,
                crop,
                &visual_dir.join(format!("{}-exact.png", form.label())),
            )?;
            if form == FontForm::CompactType0 {
                save_crop(&truth_render, crop, &visual_dir.join("truth.png"))?;
            }
        }
        let program_bytes = program.stats.as_ref().map_or(0, |stats| stats.bytes);
        let glyph_slots = program
            .stats
            .as_ref()
            .map_or(0, |stats| usize::from(stats.glyph_slots));
        variants.push(VariantReport {
            form: form.label().to_string(),
            applicable: true,
            reason: None,
            program: program.stats,
            pdf_bytes: Some(pdf.len()),
            modeled_live_bytes: Some(
                full_bytes
                    .len()
                    .saturating_add(program_bytes)
                    .saturating_add(pdf.len())
                    .saturating_add(glyph_slots.saturating_mul(2)),
            ),
            preparation_peak_rss_bytes: program.peak_rss_bytes,
            extraction_parity,
            max_origin_error_pt,
            exact_phase: Some(exact_phase),
            best_phase: Some(best_phase),
            phase_mean_ssim: Some(phase_mean_ssim),
            phase_mean_ssim_unweighted: Some(phase_mean),
            scores,
        });
    }
    let _ = std::fs::remove_file(truth_program_path);
    Ok(SampleReport {
        case: sample.case.to_string(),
        page: sample.page,
        truth_family: sample.truth_family.to_string(),
        truth_font_subtype: record.font_subtype.clone(),
        truth_font_encoding: record.font_encoding.clone(),
        size_pt: record.tf_pt,
        text: record.unicode.clone(),
        glyphs: placed.len(),
        crop_px: crop,
        truth_page_px: [truth_render.width(), truth_render.height()],
        target_lines_on_page,
        source_truth_width_matches: width_matches,
        source_truth_outline_matches: outline_matches,
        source_truth_comparable_glyphs: comparable,
        truth_program: truth_stats,
        variants,
    })
}

fn phases() -> Vec<[f64; 2]> {
    PHASES_PT
        .into_iter()
        .flat_map(|y| PHASES_PT.into_iter().map(move |x| [x, y]))
        .collect()
}

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/corpus")
}

fn run_fonttools(python: &Path, args: &[std::ffi::OsString]) -> Result<FontToolsReport> {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/fonttools_font_forms.py");
    let output = Command::new(python)
        .arg(script)
        .args(args)
        .output()
        .with_context(|| format!("run FontTools bridge with {}", python.display()))?;
    if !output.status.success() {
        bail!(
            "FontTools bridge failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse FontTools bridge report")
}

fn select_record<'a>(
    content: &'a probe::PdfPageContent,
    sample: &SampleSpec,
    face: &Face<'_>,
) -> Result<&'a probe::TextShowRecord> {
    content
        .text
        .iter()
        .filter(|record| record.font_family.as_deref() == Some(sample.truth_family))
        .filter(|record| (record.tf_pt - sample.size_pt).abs() < 0.01)
        .filter(|record| axis_aligned(record))
        .filter(|record| {
            !record.glyphs.is_empty()
                && record.glyphs.iter().all(|glyph| {
                    let mut characters = glyph.unicode.chars();
                    let Some(character) = characters.next() else {
                        return false;
                    };
                    characters.next().is_none() && face.glyph_index(character).is_some()
                })
        })
        .max_by_key(|record| {
            record
                .glyphs
                .iter()
                .filter(|glyph| !glyph.unicode.chars().all(char::is_whitespace))
                .count()
        })
        .ok_or_else(|| {
            anyhow!(
                "no usable {} {}pt record on {} page {}",
                sample.truth_family,
                sample.size_pt,
                sample.case,
                sample.page
            )
        })
}

fn axis_aligned(record: &probe::TextShowRecord) -> bool {
    let close = |left: f64, right: f64| (left - right).abs() < 1e-6;
    close(record.tm[0], 1.0)
        && close(record.tm[1], 0.0)
        && close(record.tm[2], 0.0)
        && close(record.tm[3], 1.0)
        && close(record.ctm[0], 1.0)
        && close(record.ctm[1], 0.0)
        && close(record.ctm[2], 0.0)
        && close(record.ctm[3], 1.0)
}

fn placed_glyphs(record: &probe::TextShowRecord, face: &Face<'_>) -> Result<Vec<PlacedGlyph>> {
    let mut x = record.origin[0];
    let mut placed = Vec::with_capacity(record.glyphs.len());
    for glyph in &record.glyphs {
        let mut characters = glyph.unicode.chars();
        let character = characters
            .next()
            .context("selected glyph has no Unicode scalar")?;
        if characters.next().is_some() {
            bail!("selected glyph maps to more than one Unicode scalar");
        }
        let source_gid = face
            .glyph_index(character)
            .with_context(|| format!("legal source lacks {character:?}"))?
            .0;
        placed.push(PlacedGlyph {
            character,
            source_gid,
            x_pt: x,
            y_pt: record.origin[1],
        });
        x += glyph.resulting_advance_text_units / 1_000.0 * record.tf_pt;
    }
    Ok(placed)
}

fn compact_subset(
    full_bytes: &[u8],
    placed: &[PlacedGlyph],
) -> Result<(Vec<u8>, BTreeMap<u16, u16>)> {
    let mut db = FontDb::new();
    db.register(full_bytes.to_vec())?;
    let face = db.faces().first().context("registered font has no face")?;
    let used = placed
        .iter()
        .map(|glyph| GlyphId(glyph.source_gid))
        .collect::<BTreeSet<_>>();
    let chars = placed
        .iter()
        .map(|glyph| (GlyphId(glyph.source_gid), glyph.character))
        .collect::<BTreeMap<_, _>>();
    let subset = subset_for_pdf(face, &used, &chars)?;
    Ok((subset.data, subset.remap))
}

fn candidate_pdf(
    form: FontForm,
    program: &CandidateProgram<'_>,
    placed: &[PlacedGlyph],
    face: &Face<'_>,
    size_pt: f64,
    page_size: [f64; 4],
    phases: &[[f64; 2]],
) -> Result<Vec<u8>> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font_id = if form == FontForm::OutlinePaths {
        None
    } else {
        Some(add_font(&mut document, form, program, placed, face)?)
    };
    let resources = font_id.map_or_else(Dictionary::new, |font| {
        dictionary! { "Font" => dictionary! { "F1" => font } }
    });
    let resources_id = document.add_object(resources);
    let mut page_ids = Vec::new();
    for phase in phases {
        let content = if form == FontForm::OutlinePaths {
            outline_content(placed, face, size_pt, *phase)?
        } else {
            text_content(form, program, placed, size_pt, *phase)?
        };
        let mut stream = Stream::new(dictionary! {}, content);
        stream.compress()?;
        let content_id = document.add_object(stream);
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => page_size.iter().copied().map(pdf_number).collect::<Vec<_>>(),
            "Resources" => resources_id,
            "Contents" => content_id,
        });
        page_ids.push(page_id);
    }
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
        }),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog);
    let mut pdf = Vec::new();
    document.save_to(&mut pdf)?;
    Ok(pdf)
}

fn add_font(
    document: &mut Document,
    form: FontForm,
    program: &CandidateProgram<'_>,
    placed: &[PlacedGlyph],
    face: &Face<'_>,
) -> Result<lopdf::ObjectId> {
    let data = program.data.context("text form requires a font program")?;
    let mut file = Stream::new(
        dictionary! { "Length1" => data.len() as i64 },
        data.to_vec(),
    );
    file.compress()?;
    let file_id = document.add_object(file);
    let bbox = face.global_bounding_box();
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Task5Candidate",
        "Flags" => 32,
        "FontBBox" => vec![bbox.x_min.into(), bbox.y_min.into(), bbox.x_max.into(), bbox.y_max.into()],
        "ItalicAngle" => 0,
        "Ascent" => face.ascender() as i64,
        "Descent" => face.descender() as i64,
        "CapHeight" => face.capital_height().unwrap_or(face.ascender()) as i64,
        "StemV" => 80,
        "FontFile2" => file_id,
    });
    if form == FontForm::SimpleWinAnsi {
        let widths = (0u16..=255)
            .map(|code| {
                let width = winansi_decode(code as u8)
                    .and_then(|character| face.glyph_index(character))
                    .and_then(|gid| face.glyph_hor_advance(gid))
                    .map_or(0, |width| width_1000(face, width));
                Object::Integer(i64::from(width))
            })
            .collect::<Vec<_>>();
        let cmap = to_unicode_cmap(placed, form, &program.remap);
        let mut cmap_stream = Stream::new(dictionary! {}, cmap);
        cmap_stream.compress()?;
        let cmap_id = document.add_object(cmap_stream);
        return Ok(document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Task5Candidate",
            "Encoding" => "WinAnsiEncoding",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => widths,
            "FontDescriptor" => descriptor,
            "ToUnicode" => cmap_id,
        }));
    }

    let mut widths = Vec::new();
    let mut seen = BTreeSet::new();
    for glyph in placed {
        let code = *program
            .remap
            .get(&glyph.source_gid)
            .context("candidate remap lacks used glyph")?;
        if seen.insert(code) {
            let width = face
                .glyph_hor_advance(GlyphId(glyph.source_gid))
                .map_or(0, |value| width_1000(face, value));
            widths.push(Object::Integer(i64::from(code)));
            widths.push(Object::Array(vec![Object::Integer(i64::from(width))]));
        }
    }
    let descendant = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "Task5Candidate",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor,
        "CIDToGIDMap" => "Identity",
        "DW" => 1000,
        "W" => widths,
    });
    let cmap = to_unicode_cmap(placed, form, &program.remap);
    let mut cmap_stream = Stream::new(dictionary! {}, cmap);
    cmap_stream.compress()?;
    let cmap_id = document.add_object(cmap_stream);
    Ok(document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "Task5Candidate",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![descendant.into()],
        "ToUnicode" => cmap_id,
    }))
}

fn text_content(
    form: FontForm,
    program: &CandidateProgram<'_>,
    placed: &[PlacedGlyph],
    size_pt: f64,
    [phase_x, phase_y]: [f64; 2],
) -> Result<Vec<u8>> {
    let mut operations = Vec::with_capacity(placed.len() * 4);
    for glyph in placed {
        let encoded = if form == FontForm::SimpleWinAnsi {
            vec![winansi_encode(glyph.character).context("non-WinAnsi candidate glyph")?]
        } else {
            program
                .remap
                .get(&glyph.source_gid)
                .context("candidate remap lacks used glyph")?
                .to_be_bytes()
                .to_vec()
        };
        operations.push(Operation::new("BT", Vec::new()));
        operations.push(Operation::new(
            "Tf",
            vec![Object::Name(b"F1".to_vec()), pdf_number(size_pt)],
        ));
        operations.push(Operation::new(
            "Tm",
            vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                pdf_number(glyph.x_pt + phase_x),
                pdf_number(glyph.y_pt + phase_y),
            ],
        ));
        operations.push(Operation::new(
            "Tj",
            vec![Object::String(encoded, StringFormat::Hexadecimal)],
        ));
        operations.push(Operation::new("ET", Vec::new()));
    }
    Ok(Content { operations }.encode()?)
}

fn outline_content(
    placed: &[PlacedGlyph],
    face: &Face<'_>,
    size_pt: f64,
    [phase_x, phase_y]: [f64; 2],
) -> Result<Vec<u8>> {
    let mut operations = Vec::new();
    let scale = size_pt / f64::from(face.units_per_em());
    for glyph in placed {
        let mut builder = LopdfOutline {
            operations: &mut operations,
            origin_x: glyph.x_pt + phase_x,
            origin_y: glyph.y_pt + phase_y,
            scale,
            current: (0.0, 0.0),
            contour_start: (0.0, 0.0),
        };
        if face
            .outline_glyph(GlyphId(glyph.source_gid), &mut builder)
            .is_some()
        {
            operations.push(Operation::new("f", Vec::new()));
        }
    }
    Ok(Content { operations }.encode()?)
}

struct LopdfOutline<'a> {
    operations: &'a mut Vec<Operation>,
    origin_x: f64,
    origin_y: f64,
    scale: f64,
    current: (f64, f64),
    contour_start: (f64, f64),
}

impl LopdfOutline<'_> {
    fn point(&self, x: f32, y: f32) -> (f64, f64) {
        (
            self.origin_x + f64::from(x) * self.scale,
            self.origin_y + f64::from(y) * self.scale,
        )
    }
}

impl OutlineBuilder for LopdfOutline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.operations.push(Operation::new(
            "m",
            vec![pdf_number(point.0), pdf_number(point.1)],
        ));
        self.current = point;
        self.contour_start = point;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.operations.push(Operation::new(
            "l",
            vec![pdf_number(point.0), pdf_number(point.1)],
        ));
        self.current = point;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let end = self.point(x, y);
        let first = (
            self.current.0 + (control.0 - self.current.0) * (2.0 / 3.0),
            self.current.1 + (control.1 - self.current.1) * (2.0 / 3.0),
        );
        let second = (
            end.0 + (control.0 - end.0) * (2.0 / 3.0),
            end.1 + (control.1 - end.1) * (2.0 / 3.0),
        );
        self.operations.push(Operation::new(
            "c",
            vec![
                pdf_number(first.0),
                pdf_number(first.1),
                pdf_number(second.0),
                pdf_number(second.1),
                pdf_number(end.0),
                pdf_number(end.1),
            ],
        ));
        self.current = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.point(x1, y1);
        let second = self.point(x2, y2);
        let end = self.point(x, y);
        self.operations.push(Operation::new(
            "c",
            vec![
                pdf_number(first.0),
                pdf_number(first.1),
                pdf_number(second.0),
                pdf_number(second.1),
                pdf_number(end.0),
                pdf_number(end.1),
            ],
        ));
        self.current = end;
    }

    fn close(&mut self) {
        self.operations.push(Operation::new("h", Vec::new()));
        self.current = self.contour_start;
    }
}

fn to_unicode_cmap(placed: &[PlacedGlyph], form: FontForm, remap: &BTreeMap<u16, u16>) -> Vec<u8> {
    let mut mappings = BTreeMap::new();
    for glyph in placed {
        let code = if form == FontForm::SimpleWinAnsi {
            u16::from(winansi_encode(glyph.character).unwrap_or(0))
        } else {
            remap.get(&glyph.source_gid).copied().unwrap_or(0)
        };
        mappings.entry(code).or_insert(glyph.character);
    }
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Task5ToUnicode def\n/CMapType 2 def\n1 begincodespacerange\n",
    );
    if form == FontForm::SimpleWinAnsi {
        cmap.push_str("<00> <FF>\n");
    } else {
        cmap.push_str("<0000> <FFFF>\n");
    }
    cmap.push_str("endcodespacerange\n");
    for chunk in mappings.into_iter().collect::<Vec<_>>().chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (code, character) in chunk {
            let encoded = character
                .encode_utf16(&mut [0; 2])
                .iter()
                .map(|unit| format!("{unit:04X}"))
                .collect::<String>();
            if form == FontForm::SimpleWinAnsi {
                cmap.push_str(&format!("<{code:02X}> <{encoded}>\n"));
            } else {
                cmap.push_str(&format!("<{code:04X}> <{encoded}>\n"));
            }
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

fn winansi_encode(character: char) -> Option<u8> {
    match character {
        '\u{20}'..='\u{7e}' | '\u{a0}'..='\u{ff}' => Some(character as u8),
        '\u{20ac}' => Some(0x80),
        '\u{201a}' => Some(0x82),
        '\u{192}' => Some(0x83),
        '\u{201e}' => Some(0x84),
        '\u{2026}' => Some(0x85),
        '\u{2020}' => Some(0x86),
        '\u{2021}' => Some(0x87),
        '\u{2c6}' => Some(0x88),
        '\u{2030}' => Some(0x89),
        '\u{160}' => Some(0x8a),
        '\u{2039}' => Some(0x8b),
        '\u{152}' => Some(0x8c),
        '\u{17d}' => Some(0x8e),
        '\u{2018}' => Some(0x91),
        '\u{2019}' => Some(0x92),
        '\u{201c}' => Some(0x93),
        '\u{201d}' => Some(0x94),
        '\u{2022}' => Some(0x95),
        '\u{2013}' => Some(0x96),
        '\u{2014}' => Some(0x97),
        '\u{2dc}' => Some(0x98),
        '\u{2122}' => Some(0x99),
        '\u{161}' => Some(0x9a),
        '\u{203a}' => Some(0x9b),
        '\u{153}' => Some(0x9c),
        '\u{17e}' => Some(0x9e),
        '\u{178}' => Some(0x9f),
        _ => None,
    }
}

fn winansi_decode(code: u8) -> Option<char> {
    match code {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(u32::from(code)),
        0x80 => Some('\u{20ac}'),
        0x82 => Some('\u{201a}'),
        0x83 => Some('\u{192}'),
        0x84 => Some('\u{201e}'),
        0x85 => Some('\u{2026}'),
        0x86 => Some('\u{2020}'),
        0x87 => Some('\u{2021}'),
        0x88 => Some('\u{2c6}'),
        0x89 => Some('\u{2030}'),
        0x8a => Some('\u{160}'),
        0x8b => Some('\u{2039}'),
        0x8c => Some('\u{152}'),
        0x8e => Some('\u{17d}'),
        0x91 => Some('\u{2018}'),
        0x92 => Some('\u{2019}'),
        0x93 => Some('\u{201c}'),
        0x94 => Some('\u{201d}'),
        0x95 => Some('\u{2022}'),
        0x96 => Some('\u{2013}'),
        0x97 => Some('\u{2014}'),
        0x98 => Some('\u{2dc}'),
        0x99 => Some('\u{2122}'),
        0x9a => Some('\u{161}'),
        0x9b => Some('\u{203a}'),
        0x9c => Some('\u{153}'),
        0x9e => Some('\u{17e}'),
        0x9f => Some('\u{178}'),
        _ => None,
    }
}

fn pdf_number(value: f64) -> Object {
    Object::Real(value as f32)
}

fn width_1000(face: &Face<'_>, width: u16) -> u16 {
    ((f64::from(width) / f64::from(face.units_per_em()) * 1_000.0).round() as u16).min(3_000)
}

fn glyph_crop(
    placed: &[PlacedGlyph],
    face: &Face<'_>,
    size_pt: f64,
    page_size: [f64; 4],
    rendered: &DynamicImage,
) -> Result<[u32; 4]> {
    let scale = size_pt / f64::from(face.units_per_em());
    let mut bounds: Option<[f64; 4]> = None;
    for glyph in placed {
        let Some(glyph_box) = face.glyph_bounding_box(GlyphId(glyph.source_gid)) else {
            continue;
        };
        let current = [
            glyph.x_pt + f64::from(glyph_box.x_min) * scale,
            glyph.y_pt + f64::from(glyph_box.y_min) * scale,
            glyph.x_pt + f64::from(glyph_box.x_max) * scale,
            glyph.y_pt + f64::from(glyph_box.y_max) * scale,
        ];
        bounds = Some(match bounds {
            Some(old) => [
                old[0].min(current[0]),
                old[1].min(current[1]),
                old[2].max(current[2]),
                old[3].max(current[3]),
            ],
            None => current,
        });
    }
    let [x0, y0, x1, y1] = bounds.context("selected record has no outlined glyphs")?;
    let page_width = page_size[2] - page_size[0];
    let page_height = page_size[3] - page_size[1];
    let x_scale = f64::from(rendered.width()) / page_width;
    let y_scale = f64::from(rendered.height()) / page_height;
    let padding_pt = 2.0;
    let left = ((x0 - padding_pt - 0.25) * x_scale).floor().max(0.0) as u32;
    let top = ((page_height - y1 - padding_pt - 0.25) * y_scale)
        .floor()
        .max(0.0) as u32;
    let right = ((x1 + padding_pt + 0.25) * x_scale)
        .ceil()
        .min(f64::from(rendered.width())) as u32;
    let bottom = ((page_height - y0 + padding_pt + 0.25) * y_scale)
        .ceil()
        .min(f64::from(rendered.height())) as u32;
    Ok([
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    ])
}

fn save_crop(image: &DynamicImage, [x, y, width, height]: [u32; 4], path: &Path) -> Result<()> {
    let crop = image.crop_imm(x, y, width, height);
    let mut bytes = Vec::new();
    crop.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

fn candidate_text_checks(
    pdf: &[u8],
    page: u32,
    truth: &probe::TextShowRecord,
    placed: &[PlacedGlyph],
) -> Result<(Option<bool>, Option<f64>)> {
    let content = probe::extract_content_page(pdf, page)?;
    let extracted = content
        .text
        .iter()
        .map(|record| record.unicode.as_str())
        .collect::<String>();
    let max_error = content
        .text
        .iter()
        .zip(placed)
        .map(|(record, expected)| {
            (record.origin[0] - expected.x_pt)
                .abs()
                .max((record.origin[1] - expected.y_pt).abs())
        })
        .fold(0.0, f64::max);
    Ok((
        Some(extracted == truth.unicode && content.text.len() == placed.len()),
        Some(max_error),
    ))
}

fn target_line_count(content: &probe::PdfPageContent, sample: &SampleSpec) -> usize {
    content
        .text
        .iter()
        .filter(|record| record.font_family.as_deref() == Some(sample.truth_family))
        .filter(|record| (record.tf_pt - sample.size_pt).abs() < 0.01)
        .map(|record| (record.origin[1] * 4.0).round() as i64)
        .collect::<BTreeSet<_>>()
        .len()
}

fn source_truth_matches(
    truth_record: &probe::TextShowRecord,
    placed: &[PlacedGlyph],
    source: &Face<'_>,
    truth: &Face<'_>,
) -> (usize, usize, usize) {
    let mut widths = 0usize;
    let mut outlines = 0usize;
    let mut comparable = 0usize;
    for (record, placed) in truth_record.glyphs.iter().zip(placed) {
        let Some(truth_gid) = record.source_gid else {
            continue;
        };
        comparable += 1;
        let source_width = source
            .glyph_hor_advance(GlyphId(placed.source_gid))
            .map(|value| width_1000(source, value));
        let truth_width = truth
            .glyph_hor_advance(GlyphId(truth_gid))
            .map(|value| width_1000(truth, value));
        if source_width == truth_width {
            widths += 1;
        }
        if outline_signature(source, GlyphId(placed.source_gid))
            == outline_signature(truth, GlyphId(truth_gid))
        {
            outlines += 1;
        }
    }
    (widths, outlines, comparable)
}

#[derive(Default, PartialEq, Eq)]
struct OutlineSignature(Vec<u8>);

impl OutlineSignature {
    fn push(&mut self, tag: u8, values: &[f32]) {
        self.0.push(tag);
        for value in values {
            self.0.extend_from_slice(&value.to_bits().to_be_bytes());
        }
    }
}

impl OutlineBuilder for OutlineSignature {
    fn move_to(&mut self, x: f32, y: f32) {
        self.push(b'm', &[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(b'l', &[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.push(b'q', &[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.push(b'c', &[x1, y1, x2, y2, x, y]);
    }
    fn close(&mut self) {
        self.push(b'h', &[]);
    }
}

fn outline_signature(face: &Face<'_>, gid: GlyphId) -> Option<OutlineSignature> {
    let mut signature = OutlineSignature::default();
    face.outline_glyph(gid, &mut signature)?;
    Some(signature)
}

pub(crate) fn embedded_font_program(pdf: &[u8], page: u32, resource_name: &str) -> Result<Vec<u8>> {
    let document = Document::load_mem(pdf)?;
    embedded_font_program_from_document(&document, page, resource_name)
}

pub(crate) fn embedded_font_program_from_document(
    document: &Document,
    page: u32,
    resource_name: &str,
) -> Result<Vec<u8>> {
    let page_id = *document
        .get_pages()
        .get(&page)
        .with_context(|| format!("truth PDF has no page {page}"))?;
    let resources = inherited_dictionary(document, page_id, b"Resources")?;
    let fonts = resolve(document, resources.get(b"Font")?)?.as_dict()?;
    let font = resolve(document, fonts.get(resource_name.as_bytes())?)?.as_dict()?;
    let descriptor_owner = if font.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Type0")
    {
        let descendants = resolve(document, font.get(b"DescendantFonts")?)?.as_array()?;
        resolve(
            document,
            descendants
                .first()
                .context("Type0 font has no descendant")?,
        )?
        .as_dict()?
    } else {
        font
    };
    let descriptor = resolve(document, descriptor_owner.get(b"FontDescriptor")?)?.as_dict()?;
    let file = descriptor
        .get(b"FontFile2")
        .context("truth font has no TrueType FontFile2")?;
    let stream = resolve(document, file)?.as_stream()?;
    Ok(stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone()))
}

fn pdf_page_size(pdf: &[u8], page: u32) -> Result<[f64; 4]> {
    let document = Document::load_mem(pdf)?;
    let page_id = *document
        .get_pages()
        .get(&page)
        .with_context(|| format!("PDF has no page {page}"))?;
    let media_box = inherited_object(&document, page_id, b"MediaBox")?.as_array()?;
    let values = media_box
        .iter()
        .map(pdf_f64)
        .collect::<Option<Vec<_>>>()
        .context("MediaBox contains a non-number")?;
    <[f64; 4]>::try_from(values).map_err(|_| anyhow!("MediaBox must have four numbers"))
}

fn inherited_dictionary<'a>(
    document: &'a Document,
    page_id: lopdf::ObjectId,
    key: &[u8],
) -> Result<&'a Dictionary> {
    inherited_object(document, page_id, key)?
        .as_dict()
        .map_err(Into::into)
}

fn inherited_object<'a>(
    document: &'a Document,
    mut object_id: lopdf::ObjectId,
    key: &[u8],
) -> Result<&'a Object> {
    loop {
        let dictionary = document.get_object(object_id)?.as_dict()?;
        if let Ok(value) = dictionary.get(key) {
            return resolve(document, value);
        }
        object_id = dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
            .context("page tree lacks inherited value")?;
    }
}

fn resolve<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).map_err(Into::into),
        _ => Ok(object),
    }
}

fn pdf_f64(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn aggregate(samples: &[SampleReport]) -> Vec<AggregateReport> {
    let compact_exact = mean_variant(samples, "compact_type0", |variant| {
        variant.exact_phase.as_ref().map(|score| score.ssim)
    });
    let compact_best = mean_variant(samples, "compact_type0", |variant| {
        variant.best_phase.as_ref().map(|score| score.ssim)
    });
    let compact_exact_unweighted = mean_variant(samples, "compact_type0", |variant| {
        variant
            .exact_phase
            .as_ref()
            .map(|score| score.ssim_unweighted)
    });
    let compact_best_unweighted = mean_variant(samples, "compact_type0", |variant| {
        variant
            .best_phase
            .as_ref()
            .map(|score| score.ssim_unweighted)
    });
    FontForm::ALL
        .into_iter()
        .map(|form| {
            let label = form.label();
            let applicable = samples
                .iter()
                .filter_map(|sample| sample.variants.iter().find(|variant| variant.form == label))
                .filter(|variant| variant.applicable)
                .count();
            let exact = mean_variant(samples, label, |variant| {
                variant.exact_phase.as_ref().map(|score| score.ssim)
            });
            let best = mean_variant(samples, label, |variant| {
                variant.best_phase.as_ref().map(|score| score.ssim)
            });
            let exact_unweighted = mean_variant(samples, label, |variant| {
                variant
                    .exact_phase
                    .as_ref()
                    .map(|score| score.ssim_unweighted)
            });
            let best_unweighted = mean_variant(samples, label, |variant| {
                variant
                    .best_phase
                    .as_ref()
                    .map(|score| score.ssim_unweighted)
            });
            let mean_program = mean_variant(samples, label, |variant| {
                variant.program.as_ref().map(|program| program.bytes as f64)
            });
            let mean_pdf = mean_variant(samples, label, |variant| {
                variant.pdf_bytes.map(|bytes| bytes as f64)
            });
            let estimated_page_delta = mean_page_delta(samples, label);
            AggregateReport {
                form: label.to_string(),
                applicable_samples: applicable,
                exact_mean_ssim: exact,
                exact_delta_from_compact: exact.zip(compact_exact).map(|(a, b)| a - b),
                exact_mean_ssim_unweighted: exact_unweighted,
                exact_delta_from_compact_unweighted: exact_unweighted
                    .zip(compact_exact_unweighted)
                    .map(|(a, b)| a - b),
                best_phase_mean_ssim: best,
                best_phase_delta_from_compact: best.zip(compact_best).map(|(a, b)| a - b),
                best_phase_mean_ssim_unweighted: best_unweighted,
                best_phase_delta_from_compact_unweighted: best_unweighted
                    .zip(compact_best_unweighted)
                    .map(|(a, b)| a - b),
                mean_program_bytes: mean_program,
                mean_pdf_bytes: mean_pdf,
                estimated_page_delta_unweighted_area_model: estimated_page_delta,
            }
        })
        .collect()
}

fn mean_variant(
    samples: &[SampleReport],
    label: &str,
    value: impl Fn(&VariantReport) -> Option<f64>,
) -> Option<f64> {
    let values = samples
        .iter()
        .filter_map(|sample| sample.variants.iter().find(|variant| variant.form == label))
        .filter_map(value)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn mean_page_delta(samples: &[SampleReport], label: &str) -> Option<f64> {
    let deltas = samples
        .iter()
        .filter_map(|sample| {
            let compact = sample
                .variants
                .iter()
                .find(|variant| variant.form == "compact_type0")?
                .exact_phase
                .as_ref()?
                .ssim_unweighted;
            let candidate = sample
                .variants
                .iter()
                .find(|variant| variant.form == label)?
                .exact_phase
                .as_ref()?
                .ssim_unweighted;
            let crop_windows = (sample.crop_px[2] / 8) * (sample.crop_px[3] / 8);
            let page_windows = (sample.truth_page_px[0] / 8) * (sample.truth_page_px[1] / 8);
            (page_windows > 0).then(|| {
                (candidate - compact) * f64::from(crop_windows) / f64::from(page_windows)
                    * sample.target_lines_on_page as f64
            })
        })
        .collect::<Vec<_>>();
    (!deltas.is_empty()).then(|| deltas.iter().sum::<f64>() / deltas.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_grid_includes_exact_origin_and_four_phases_per_axis() {
        let phases = phases();
        assert_eq!(phases.len(), 16);
        assert!(phases.contains(&[0.0, 0.0]));
    }

    #[test]
    fn winansi_cmap_uses_one_byte_source_codes() {
        let glyph = PlacedGlyph {
            character: 'A',
            source_gid: 7,
            x_pt: 0.0,
            y_pt: 0.0,
        };
        let cmap = String::from_utf8(to_unicode_cmap(
            &[glyph],
            FontForm::SimpleWinAnsi,
            &BTreeMap::new(),
        ))
        .expect("CMap is ASCII");
        assert!(cmap.contains("<41> <0041>"));
    }

    #[test]
    fn winansi_supports_cp1252_punctuation() {
        assert_eq!(winansi_encode('\u{2019}'), Some(0x92));
        assert_eq!(winansi_decode(0x92), Some('\u{2019}'));
        assert_eq!(winansi_encode('\u{4e2d}'), None);
    }
}
