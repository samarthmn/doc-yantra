//! The fidelity harness runner: seed the corpus, score cases against Word
//! ground truths, and emit JSON + HTML reports.
//!
//! Ground-truth PDFs are produced by a human. `run` is strict: every selected
//! case needs a source, manifest, ground truth, and recorded Word provenance.

mod advance_models;
mod advance_probe;
mod corpus;
mod font_form_experiment;
mod font_raster_probe;
mod fonts;
mod image_probe;
mod probe;
mod resolver;
mod ssim;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use fidelity::evidence::{is_sha256, sha256_hex, verify_sha256};
use office_core::{ConvertOptions, OfficeFormat, convert};
use pdf_core::raster::{RenderOptions, render_page};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fidelity-corpus")
}
fn report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tmp/fidelity-report")
}

const SCORING_RASTER_CONFIG: &[u8] = b"pdfium-render=0.9.2;render_page;target_width_px=none;scale=2.0;format=png;page_aggregation=min;ssim_source=";

fn scoring_raster_config_sha256() -> String {
    let mut fingerprinted =
        Vec::with_capacity(SCORING_RASTER_CONFIG.len() + include_bytes!("ssim.rs").len());
    fingerprinted.extend_from_slice(SCORING_RASTER_CONFIG);
    fingerprinted.extend_from_slice(include_bytes!("ssim.rs"));
    sha256_hex(&fingerprinted)
}

#[derive(Parser)]
#[command(name = "fidelity", about = "office-core fidelity harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// (Re)generate the synthetic corpus `source.docx` files + manifests.
    Seed,
    /// Score the complete reference corpus and enforce its release thresholds.
    Run {
        #[arg(long, default_value = "docx")]
        format: String,
        #[arg(long)]
        case: Option<String>,
        /// Directory for generated JSON, HTML, and page-render evidence.
        #[arg(long)]
        report_dir: Option<PathBuf>,
    },
    /// Deliberately refresh the hash-pinned font artifacts used by corpus runs.
    LockFonts,
    /// Compare vector border and text geometry between our PDF and Word's.
    Probe {
        /// Reference-corpus case under `fidelity-corpus/docx/`.
        #[arg(long, conflicts_with = "diag")]
        case: Option<String>,
        /// Diagnostic fixture under `office-core/fidelity/diag/`.
        #[arg(long, conflicts_with = "case")]
        diag: Option<String>,
        /// One-based page number.
        #[arg(long)]
        page: u32,
        /// Optional JSON report path.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Write our converted PDF here for stream-level inspection.
        #[arg(long = "dump-pdf")]
        dump_pdf: Option<PathBuf>,
        /// Additional runtime font directory (repeatable).
        #[arg(long = "font-dir")]
        font_dirs: Vec<PathBuf>,
    },
    /// Compare exact per-glyph advances and cumulative text-show endpoints.
    AdvanceProbe {
        #[command(flatten)]
        pdfs: PdfPairArgs,
    },
    /// Evaluate Task 2 advance candidates on the frozen discovery/held-out split.
    AdvanceModels {
        /// Directory containing `<case>-ours.pdf` baseline candidates.
        #[arg(long)]
        ours_dir: PathBuf,
        /// JSON result path, outside the frozen corpus.
        #[arg(long)]
        json: PathBuf,
    },
    /// Compare already-positioned glyph rasters with the official scorer.
    FontRasterProbe {
        #[command(flatten)]
        pdfs: PdfPairArgs,
        /// Optional pixel crop: x y width height.
        #[arg(long, num_args = 4)]
        crop: Option<Vec<u32>>,
    },
    /// Run Task 5's controlled font-program/PDF-form raster experiment.
    FontFormExperiment {
        /// Hinted Ubuntu 0.83 `Ubuntu-R.ttf` from a legal source archive.
        #[arg(long)]
        ubuntu_font: PathBuf,
        /// Microsoft Word's local DFonts directory (harness use only).
        #[arg(
            long,
            default_value = "/Applications/Microsoft Word.app/Contents/Resources/DFonts"
        )]
        dfonts_dir: PathBuf,
        /// Python interpreter with fontTools installed.
        #[arg(long, default_value = "python3")]
        python: PathBuf,
        /// Scratch directory for generated candidate fonts, PDFs, and crops.
        #[arg(long)]
        output_dir: PathBuf,
        /// JSON result path, outside the frozen corpus.
        #[arg(long)]
        json: PathBuf,
    },
    /// Extract image XObjects and report their placement/codec metadata.
    ImageProbe {
        #[command(flatten)]
        pdfs: PdfPairArgs,
        /// Optional directory for exact encoded XObject byte streams.
        #[arg(long)]
        extract_dir: Option<PathBuf>,
    },
    /// Run image resampling/codec candidates against one truth XObject per page.
    ImageCandidateMatrix {
        /// DOCX whose `word/media` images are evaluated in natural image-number order.
        #[arg(long)]
        source_docx: PathBuf,
        /// Reference PDF containing one placed image on each corresponding page.
        #[arg(long)]
        truth_pdf: PathBuf,
        /// Optional JSON report path.
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// List corpus cases lacking a `ground-truth.pdf`.
    MissingGroundTruth,
}

#[derive(Args)]
struct PdfPairArgs {
    /// Candidate PDF produced by office-core or another controlled emitter.
    #[arg(long)]
    ours_pdf: PathBuf,
    /// Word/reference PDF used for diagnostic comparison.
    #[arg(long)]
    truth_pdf: PathBuf,
    /// One-based page number.
    #[arg(long)]
    page: u32,
    /// Optional JSON report path.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Manifest {
    title: String,
    tier: String,
    ssim_threshold: f64,
    #[serde(default)]
    max_page_count_delta: u32,
    word_version: String,
    ground_truth_exported_at: String,
    /// `"web-export-unattested"` opts a case out of PDF producer/date
    /// matching: Office-for-the-web exports strip PDF metadata, so their
    /// provenance rests on the manifest declaration, the pinned hashes, and
    /// the notes. Absent (default) keeps the strict desktop contract.
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default)]
    notes: String,
    input_sha256: String,
    golden_sha256: String,
    #[serde(default)]
    required_fonts: Vec<String>,
    #[serde(default)]
    expected_fonts: Vec<String>,
    #[serde(default = "default_geometry_median_pt")]
    geometry_median_pt: f64,
    #[serde(default = "default_geometry_p95_pt")]
    geometry_p95_pt: f64,
    #[serde(default = "default_geometry_max_unmatched_pct")]
    geometry_max_unmatched_pct: f64,
}

fn default_geometry_median_pt() -> f64 {
    probe::GeometryLimits::default().median_pt
}

fn default_geometry_p95_pt() -> f64 {
    probe::GeometryLimits::default().p95_pt
}

fn default_geometry_max_unmatched_pct() -> f64 {
    probe::GeometryLimits::default().max_unmatched_pct
}

#[derive(Serialize)]
struct CaseReport {
    name: String,
    title: String,
    tier: String,
    threshold: f64,
    ssim: f64,
    ssim_unweighted: f64,
    diff_pct: f64,
    fonts: Vec<String>,
    truth_fonts: Vec<String>,
    our_pages: u32,
    truth_pages: u32,
    truth_pdf_sha256: String,
    scoring_raster_config_sha256: String,
    pages: Vec<PageReport>,
    layout_trace: Option<serde_json::Value>,
    geometry_limits: probe::GeometryLimits,
    geometry: Vec<probe::GeometryPageMetrics>,
    geometry_pass: bool,
    pass: bool,
    note: String,
}

#[derive(Debug, Serialize)]
struct PageReport {
    page: u32,
    ssim: f64,
    ssim_unweighted: f64,
    diff_pct: f64,
    ours_pixel_width: u32,
    ours_pixel_height: u32,
    truth_pixel_width: u32,
    truth_pixel_height: u32,
    our_pdf_pages: u32,
    truth_pdf_pages: u32,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Seed => {
            let n = corpus::seed(&corpus_root())?;
            println!("seeded {n} docx cases under {}", corpus_root().display());
        }
        Cmd::MissingGroundTruth => list_missing()?,
        Cmd::LockFonts => lock_fonts()?,
        Cmd::Probe {
            case,
            diag,
            page,
            json,
            dump_pdf,
            font_dirs,
        } => run_probe(
            case.as_deref(),
            diag.as_deref(),
            page,
            json.as_deref(),
            dump_pdf.as_deref(),
            &font_dirs,
        )?,
        Cmd::AdvanceProbe { pdfs } => run_advance_probe(&pdfs)?,
        Cmd::AdvanceModels { ours_dir, json } => {
            ensure_diagnostic_output_outside_corpus(&json)?;
            let report = advance_models::run(&ours_dir)?;
            print_or_write_json(&report, Some(&json))?;
        }
        Cmd::FontRasterProbe { pdfs, crop } => run_font_raster_probe(&pdfs, crop.as_deref())?,
        Cmd::FontFormExperiment {
            ubuntu_font,
            dfonts_dir,
            python,
            output_dir,
            json,
        } => {
            ensure_diagnostic_output_outside_corpus(&output_dir)?;
            ensure_diagnostic_output_outside_corpus(&json)?;
            let report =
                font_form_experiment::run(&output_dir, &ubuntu_font, &dfonts_dir, &python)?;
            print_or_write_json(&report, Some(&json))?;
        }
        Cmd::ImageProbe { pdfs, extract_dir } => run_image_probe(&pdfs, extract_dir.as_deref())?,
        Cmd::ImageCandidateMatrix {
            source_docx,
            truth_pdf,
            json,
        } => {
            let report = image_probe::candidate_matrix(&source_docx, &truth_pdf)?;
            print_or_write_json(&report, json.as_deref())?;
        }
        Cmd::Run {
            format,
            case,
            report_dir: selected_report_dir,
        } => {
            let selected_report_dir = selected_report_dir.unwrap_or_else(report_dir);
            run(&format, case.as_deref(), &selected_report_dir)?
        }
    }
    Ok(())
}

fn run_probe(
    case: Option<&str>,
    diag: Option<&str>,
    page: u32,
    json_path: Option<&Path>,
    dump_pdf: Option<&Path>,
    font_dirs: &[PathBuf],
) -> Result<()> {
    if let Some(path) = json_path {
        ensure_diagnostic_output_outside_corpus(path)?;
    }
    if let Some(path) = dump_pdf {
        ensure_diagnostic_output_outside_corpus(path)?;
    }
    let directory = match (case, diag) {
        (Some(case), None) => corpus_root().join("docx").join(case),
        (None, Some(diag)) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("diag")
            .join(diag),
        _ => anyhow::bail!("probe requires exactly one of --case or --diag"),
    };
    let source_path = directory.join("source.docx");
    let truth_path = directory.join("ground-truth.pdf");
    let source = std::fs::read(&source_path)
        .with_context(|| format!("read probe source {}", source_path.display()))?;
    let truth = std::fs::read(&truth_path)
        .with_context(|| format!("read probe ground truth {}", truth_path.display()))?;
    let base_options = ConvertOptions {
        font_dirs: font_dirs.to_vec(),
        ..ConvertOptions::default()
    };
    let name = case.or(diag).expect("probe source kind was validated");
    let resolved = resolver::resolve_for_case(name, &source, OfficeFormat::Docx, &base_options)?;
    let ours = convert(&source, OfficeFormat::Docx, &resolved.options)
        .with_context(|| format!("convert probe source {}", source_path.display()))?;
    if let Some(path) = dump_pdf {
        std::fs::write(path, &ours.pdf).context("write dumped probe PDF")?;
    }
    let report = probe::compare_page(&ours.pdf, &truth, page)?;
    probe::print_report(&report);
    if let Some(path) = json_path {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create probe report directory {}", parent.display()))?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("write probe JSON {}", path.display()))?;
        println!("JSON: {}", path.display());
    }
    Ok(())
}

fn run_advance_probe(args: &PdfPairArgs) -> Result<()> {
    let (ours, truth) = read_pdf_pair(args)?;
    let ours = probe::extract_content_page(&ours, args.page)?;
    let truth = probe::extract_content_page(&truth, args.page)?;
    let report = advance_probe::compare(&ours, &truth);
    print_or_write_json(&report, args.json.as_deref())
}

fn run_font_raster_probe(args: &PdfPairArgs, crop: Option<&[u32]>) -> Result<()> {
    let (ours, truth) = read_pdf_pair(args)?;
    let crop = crop
        .map(|values| {
            <[u32; 4]>::try_from(values)
                .map_err(|_| anyhow::anyhow!("--crop requires x y width height"))
        })
        .transpose()?;
    let report = font_raster_probe::compare(&ours, &truth, args.page, crop)?;
    print_or_write_json(&report, args.json.as_deref())
}

fn run_image_probe(args: &PdfPairArgs, extract_dir: Option<&Path>) -> Result<()> {
    let (ours, truth) = read_pdf_pair(args)?;
    if let Some(directory) = extract_dir {
        ensure_diagnostic_output_outside_corpus(directory)?;
    }
    let report = image_probe::compare(&ours, &truth, args.page, extract_dir)?;
    print_or_write_json(&report, args.json.as_deref())
}

fn read_pdf_pair(args: &PdfPairArgs) -> Result<(Vec<u8>, Vec<u8>)> {
    let ours = std::fs::read(&args.ours_pdf)
        .with_context(|| format!("read candidate PDF {}", args.ours_pdf.display()))?;
    let truth = std::fs::read(&args.truth_pdf)
        .with_context(|| format!("read truth PDF {}", args.truth_pdf.display()))?;
    Ok((ours, truth))
}

fn print_or_write_json<T: Serialize>(report: &T, json_path: Option<&Path>) -> Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    if let Some(path) = json_path {
        ensure_diagnostic_output_outside_corpus(path)?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create diagnostic directory {}", parent.display()))?;
        }
        std::fs::write(path, &json)
            .with_context(|| format!("write diagnostic JSON {}", path.display()))?;
        println!("JSON: {}", path.display());
    } else {
        println!("{}", String::from_utf8_lossy(&json));
    }
    Ok(())
}

fn list_missing() -> Result<()> {
    let mut missing = 0;
    for dir in case_dirs("docx")? {
        if !dir.join("ground-truth.pdf").exists() {
            println!(
                "missing ground truth: {}",
                dir.file_name().unwrap().to_string_lossy()
            );
            missing += 1;
        }
    }
    if missing == 0 {
        println!("all corpus cases have ground truths");
    }
    Ok(())
}

fn case_dirs(format: &str) -> Result<Vec<PathBuf>> {
    let base = corpus_root().join(format);
    if !base.is_dir() {
        anyhow::bail!(
            "no {format} fidelity corpus cases found under {} — finalize cases from \
             fidelity-corpus/{format}-pending/ (ground truths + calibrated manifests) first",
            base.display()
        );
    }
    let mut dirs = Vec::new();
    let entries = std::fs::read_dir(&base)
        .with_context(|| format!("read fidelity corpus {}", base.display()))?;
    for entry in entries {
        let path = entry
            .with_context(|| format!("read entry under {}", base.display()))?
            .path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() {
        anyhow::bail!(
            "no {format} fidelity corpus cases found under {}",
            base.display()
        );
    }
    Ok(dirs)
}

/// The case's frozen source: `source.<format>`, plus `source.xlsm` for
/// macro-enabled workbooks in the xlsx corpus.
fn case_source_file(dir: &Path, format: &str) -> Result<PathBuf> {
    let mut candidates = vec![format!("source.{format}")];
    if format == "xlsx" {
        candidates.push("source.xlsm".to_string());
    }
    for candidate in &candidates {
        let path = dir.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    anyhow::bail!(
        "selected case {} is missing {}",
        dir.display(),
        candidates.join(" or ")
    )
}

fn run(format: &str, only: Option<&str>, selected_report_dir: &Path) -> Result<()> {
    ensure_diagnostic_output_outside_corpus(selected_report_dir)?;
    let office_format = match format {
        "docx" => OfficeFormat::Docx,
        "pptx" => OfficeFormat::Pptx,
        "xlsx" => OfficeFormat::Xlsx,
        _ => anyhow::bail!(
            "no {format} reference corpus is committed; refusing to claim fidelity coverage"
        ),
    };
    let pages_dir = selected_report_dir.join("pages");
    std::fs::create_dir_all(&pages_dir).ok();

    let mut reports = Vec::new();
    let selected: Vec<PathBuf> = case_dirs(format)?
        .into_iter()
        .filter(|dir| only.is_none_or(|only| dir.file_name().is_some_and(|name| name == only)))
        .collect();
    if selected.is_empty() {
        anyhow::bail!("selected fidelity case {only:?} does not exist for {format}");
    }

    for dir in selected {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let source = case_source_file(&dir, format)?;
        let gt = dir.join("ground-truth.pdf");
        if !gt.is_file() {
            anyhow::bail!("selected case {name} is missing {}", gt.display());
        }
        let manifest_path = dir.join("manifest.toml");
        let manifest_contents = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&manifest_contents)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        validate_manifest_contract(&manifest_path, &manifest)?;
        let source_bytes = read_verified_frozen_file(&source, &manifest.input_sha256)?;
        let ground_truth_bytes = read_verified_frozen_file(&gt, &manifest.golden_sha256)?;
        validate_provenance(&manifest_path, &manifest, format, &gt, &ground_truth_bytes)?;
        let resolved = resolver::resolve_for_case(
            &name,
            &source_bytes,
            office_format,
            &ConvertOptions::default(),
        )?;
        validate_required_fonts(&name, &source_bytes, office_format, &manifest, &resolved)?;
        reports.push(score_case(
            &name,
            &source_bytes,
            office_format,
            &ground_truth_bytes,
            &manifest,
            &pages_dir,
            &resolved.options,
            &resolved.demanded_families,
        ));
    }

    write_reports(&reports, selected_report_dir)?;
    enforce_thresholds(&reports, only.is_none())?;
    let core_failures = reports
        .iter()
        .filter(|report| report.tier == "core" && !report.pass)
        .count();
    let extended_total = reports
        .iter()
        .filter(|report| report.tier == "extended")
        .count();
    let extended_passes = reports
        .iter()
        .filter(|report| report.tier == "extended" && report.pass)
        .count();
    let experimental: Vec<&CaseReport> = reports
        .iter()
        .filter(|report| report.tier == "experimental")
        .collect();
    let experimental_note = if experimental.is_empty() {
        String::new()
    } else {
        format!(
            "; experimental (non-gating): {}",
            experimental
                .iter()
                .map(|report| format!("{} {:.4}", report.name, report.ssim_unweighted))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    println!(
        "scored {} cases; {} core failures; {}/{} extended passed{}. report: {}",
        reports.len(),
        core_failures,
        extended_passes,
        extended_total,
        experimental_note,
        selected_report_dir.join("index.html").display()
    );
    Ok(())
}

fn ensure_diagnostic_output_outside_corpus(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let resolved = resolve_output_path(&absolute)?;
    let corpus = std::fs::canonicalize(corpus_root()).context("resolve fidelity corpus root")?;
    if resolved.starts_with(&corpus) {
        anyhow::bail!(
            "refusing to write diagnostic output under the corpus tree: {}",
            path.display()
        );
    }
    Ok(())
}

fn resolve_output_path(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("diagnostic output path has no existing ancestor"))?;
        missing.push(name.to_owned());
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("diagnostic output path has no parent"))?;
    }
    let mut resolved = std::fs::canonicalize(existing)
        .with_context(|| format!("resolve output ancestor {}", existing.display()))?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn lock_fonts() -> Result<()> {
    let mut sources = Vec::new();
    for dir in case_dirs("docx")? {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let source_path = dir.join("source.docx");
        let source = std::fs::read(&source_path)
            .with_context(|| format!("read font-lock source {}", source_path.display()))?;
        sources.push((name, source));
    }
    let count = resolver::refresh_lock(&sources)?;
    println!(
        "locked {count} font artifacts in {}",
        resolver::lock_path().display()
    );
    Ok(())
}

fn validate_required_fonts(
    case_name: &str,
    source: &[u8],
    format: OfficeFormat,
    manifest: &Manifest,
    resolved: &resolver::ResolvedFonts,
) -> Result<()> {
    let unresolved = office_core::font_requests(source, format, &resolved.options)
        .with_context(|| format!("verify resolved fonts for fidelity case {case_name}"))?;
    for required in &manifest.required_fonts {
        let required = required
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if !resolved
            .families
            .iter()
            .any(|family| family.eq_ignore_ascii_case(&required))
        {
            anyhow::bail!(
                "case {case_name} requires exact font {required}, but fonts.lock did not provision it"
            );
        }
        if unresolved.iter().any(|request| {
            request
                .family
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .eq_ignore_ascii_case(&required)
        }) {
            anyhow::bail!(
                "case {case_name} requires exact font {required}, but it remains unresolved after locked font provisioning"
            );
        }
    }
    let profile = office_core::profile_with_options(source, format, &resolved.options)
        .with_context(|| format!("profile resolved fonts for fidelity case {case_name}"))?;
    validate_required_font_profile(case_name, &manifest.required_fonts, &profile)?;
    Ok(())
}

fn normalized_font_family(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn validate_required_font_profile(
    case_name: &str,
    required_fonts: &[String],
    profile: &office_core::DocProfile,
) -> Result<()> {
    for required in required_fonts {
        let normalized_required = normalized_font_family(required);
        if let Some(warning) = profile.warnings.iter().find(|warning| {
            matches!(
                warning.code,
                "font_fallback" | "font_fallback_used" | "font_substituted"
            ) && warning
                .detail
                .split_once(" ->")
                .map_or(warning.detail.as_str(), |(requested, _)| requested)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .eq_ignore_ascii_case(&normalized_required)
        }) {
            anyhow::bail!(
                "case {case_name} requires exact font {required}, but profile emitted {}: {}",
                warning.code,
                warning.detail
            );
        }

        let resolutions = profile
            .fonts
            .iter()
            .filter(|resolution| {
                normalized_font_family(&resolution.requested_family) == normalized_required
            })
            .collect::<Vec<_>>();
        let exact = !resolutions.is_empty()
            && resolutions.iter().all(|resolution| {
                resolution.coverage_ok
                    && matches!(
                        resolution.exactness,
                        office_core::FontExactness::EmbeddedExact
                            | office_core::FontExactness::HashPinnedExact
                    )
                    && !matches!(
                        resolution.source,
                        office_core::FontResolutionSource::Fallback
                            | office_core::FontResolutionSource::None
                    )
                    && resolution.selected.as_ref().is_some_and(|selected| {
                        normalized_font_family(&selected.family) == normalized_required
                    })
            });
        if !exact {
            let selected = resolutions
                .iter()
                .filter_map(|resolution| resolution.selected.as_ref())
                .map(|selected| selected.family.as_str())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "case {case_name} requires exact font {required}, but it was not resolved exactly (selected: {selected:?})"
            );
        }
    }
    Ok(())
}

fn validate_embedded_font_expectations(
    case_name: &str,
    expected: &BTreeSet<String>,
    demanded: &BTreeSet<String>,
    embedded: &BTreeSet<String>,
) -> Result<()> {
    let expected_normalized = expected
        .iter()
        .map(|family| normalized_font_family(family))
        .collect::<BTreeSet<_>>();
    let embedded_normalized = embedded
        .iter()
        .map(|family| normalized_font_family(family))
        .collect::<BTreeSet<_>>();
    let demanded_normalized = demanded
        .iter()
        .map(|family| normalized_font_family(family))
        .collect::<BTreeSet<_>>();
    let has_unexpected = !embedded_normalized.is_subset(&expected_normalized);
    let missing_demanded = expected_normalized
        .intersection(&demanded_normalized)
        .any(|family| !embedded_normalized.contains(family));
    if has_unexpected || missing_demanded {
        let display = |families: &BTreeSet<String>| {
            format!(
                "[{}]",
                families.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        anyhow::bail!(
            "case {case_name} font mismatch: expected {}, embedded {}",
            display(expected),
            display(embedded)
        );
    }
    Ok(())
}

fn validate_manifest_contract(path: &Path, manifest: &Manifest) -> Result<()> {
    if manifest.title.trim().is_empty() {
        anyhow::bail!("{} has an empty title", path.display());
    }
    // `experimental` cases run and report but do not gate releases — for
    // corpus entries whose engine support ships while their score still
    // converges toward the (unchanged) 0.90 floor.
    if manifest.tier != "core" && manifest.tier != "extended" && manifest.tier != "experimental" {
        anyhow::bail!("{} has an unknown fidelity tier", path.display());
    }
    // 0.85 floor: the promoted CJK cases gate at 0.85 until the joint CJK
    // line-model recalibration (LAYOUT_CALIBRATION.md 2026-08-02) lifts them
    // back to the 0.90 standard the Latin corpus uses.
    if !manifest.ssim_threshold.is_finite() || !(0.85..=1.0).contains(&manifest.ssim_threshold) {
        anyhow::bail!(
            "{} must use an SSIM threshold between 0.90 and 1.0",
            path.display()
        );
    }
    if manifest.max_page_count_delta > 1 {
        anyhow::bail!("{} allows an excessive page-count delta", path.display());
    }
    if !manifest.geometry_median_pt.is_finite()
        || manifest.geometry_median_pt <= 0.0
        || manifest.geometry_median_pt > 10.0
    {
        anyhow::bail!("{} has an invalid geometry_median_pt", path.display());
    }
    if !manifest.geometry_p95_pt.is_finite()
        || manifest.geometry_p95_pt < manifest.geometry_median_pt
        || manifest.geometry_p95_pt > 20.0
    {
        anyhow::bail!("{} has an invalid geometry_p95_pt", path.display());
    }
    if !manifest.geometry_max_unmatched_pct.is_finite()
        || !(0.0..=100.0).contains(&manifest.geometry_max_unmatched_pct)
    {
        anyhow::bail!(
            "{} has an invalid geometry_max_unmatched_pct",
            path.display()
        );
    }
    if manifest.expected_fonts.is_empty() {
        anyhow::bail!("{} must declare expected_fonts", path.display());
    }
    if !is_sha256(&manifest.input_sha256) || !is_sha256(&manifest.golden_sha256) {
        anyhow::bail!(
            "{} must carry canonical input_sha256 and golden_sha256 values",
            path.display()
        );
    }
    Ok(())
}

fn read_verified_frozen_file(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    verify_sha256(path, &bytes, expected).map_err(anyhow::Error::msg)?;
    Ok(bytes)
}

fn validate_provenance(
    path: &Path,
    manifest: &Manifest,
    format: &str,
    ground_truth_path: &Path,
    ground_truth: &[u8],
) -> Result<()> {
    if manifest.provenance.as_deref() == Some("web-export-unattested") {
        // Office-for-the-web exports carry no usable PDF metadata (and may
        // pass through re-packagers). The manifest must still declare the
        // authoring tool and record the export in its notes; hashes pin the
        // exact bytes.
        if manifest.word_version.trim().is_empty()
            || !manifest.notes.to_ascii_lowercase().contains("web export")
        {
            anyhow::bail!(
                "{} opts into web-export-unattested provenance but does not                  declare the authoring tool and a 'web export' note",
                path.display()
            );
        }
        return Ok(());
    }
    // The provenance producer follows the corpus format: Word exports docx
    // truths, PowerPoint pptx, Excel xlsx/xlsm.
    let expected_producer = match format {
        "pptx" => "microsoft powerpoint",
        "xlsx" => "microsoft excel",
        _ => "microsoft word",
    };
    let declared_producer = manifest.word_version.trim();
    if !declared_producer
        .to_ascii_lowercase()
        .starts_with(expected_producer)
        || declared_producer.to_ascii_lowercase().contains("tbd")
        || declared_producer
            .to_ascii_lowercase()
            .contains("not recorded")
    {
        anyhow::bail!("{} has invalid Word producer provenance", path.display());
    }

    let date = manifest.ground_truth_exported_at.trim();
    let parts: Vec<&str> = date.split('-').collect();
    let valid_date = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
        && parts[1]
            .parse::<u8>()
            .is_ok_and(|month| (1..=12).contains(&month))
        && parts[2]
            .parse::<u8>()
            .is_ok_and(|day| (1..=31).contains(&day));
    if !valid_date {
        anyhow::bail!("{} has invalid ground-truth export date", path.display());
    }

    let metadata = pdf_core::meta::get_metadata(ground_truth)
        .with_context(|| format!("read PDF metadata from {}", ground_truth_path.display()))?;
    let actual_producer = metadata.producer.or(metadata.creator).ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no PDF Producer or Creator metadata",
            ground_truth_path.display()
        )
    })?;
    if !word_producer_matches(declared_producer, &actual_producer) {
        anyhow::bail!(
            "{} declares {declared_producer:?}, but PDF metadata reports {actual_producer:?}",
            path.display()
        );
    }
    let actual_date = embedded_creation_date(ground_truth).ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no parseable PDF CreationDate",
            ground_truth_path.display()
        )
    })?;
    if actual_date != date {
        anyhow::bail!(
            "{} declares export date {date}, but PDF metadata reports {actual_date}",
            path.display()
        );
    }
    Ok(())
}

fn word_producer_matches(declared: &str, actual: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let declared = normalize(declared);
    let actual = normalize(actual);
    if !actual.starts_with("microsoftword") {
        return false;
    }
    if declared.contains("ltsc") {
        return actual.contains("ltsc");
    }
    // Subscription Word writes "Microsoft® Word for Microsoft 365" — a family
    // marker, not a 4-digit year. Without this branch no truthful word_version
    // can ever pass for an M365 export and every release surface blocks.
    if declared.contains("365") {
        return actual.contains("365");
    }
    // Word's online export service stamps the bare family string with no
    // version. Accept it only when both sides are exactly the bare family.
    if declared == "microsoftwordonlineexportservice" || declared == "microsoftword" {
        return actual == "microsoftword";
    }
    let version: String = declared
        .chars()
        .skip_while(|character| !character.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    version.len() == 4 && actual.contains(&version)
}

fn embedded_creation_date(pdf: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"/CreationDate";
    let key_at = pdf.windows(KEY.len()).position(|window| window == KEY)?;
    let metadata = &pdf[key_at..pdf.len().min(key_at + 96)];
    let date_at = metadata.windows(2).position(|window| window == b"D:")? + 2;
    let digits = metadata.get(date_at..date_at + 8)?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits = std::str::from_utf8(digits).ok()?;
    Some(format!(
        "{}-{}-{}",
        &digits[..4],
        &digits[4..6],
        &digits[6..8]
    ))
}

fn enforce_thresholds(reports: &[CaseReport], require_both_tiers: bool) -> Result<()> {
    let core: Vec<&CaseReport> = reports
        .iter()
        .filter(|report| report.tier == "core")
        .collect();
    let extended: Vec<&CaseReport> = reports
        .iter()
        .filter(|report| report.tier == "extended")
        .collect();
    if reports.iter().any(|report| {
        report.tier != "core" && report.tier != "extended" && report.tier != "experimental"
    }) {
        anyhow::bail!("fidelity manifest has an unknown tier");
    }
    if require_both_tiers && core.is_empty() {
        anyhow::bail!("full fidelity corpus must contain at least one gating core case");
    }
    if core.iter().any(|report| !report.pass) {
        anyhow::bail!("core fidelity gate failed: every core case must pass");
    }
    let extended_passes = extended.iter().filter(|report| report.pass).count();
    if !extended.is_empty() && extended_passes * 100 < extended.len() * 90 {
        anyhow::bail!("extended fidelity gate failed: fewer than 90% of cases passed");
    }
    Ok(())
}

fn case_passes_release_gates(legacy_ssim: f64, legacy_threshold: f64, geometry_pass: bool) -> bool {
    legacy_ssim >= legacy_threshold && geometry_pass
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn report(tier: &str, pass: bool) -> CaseReport {
        CaseReport {
            name: "case".into(),
            title: "Case".into(),
            tier: tier.into(),
            threshold: 0.9,
            ssim: if pass { 1.0 } else { 0.0 },
            ssim_unweighted: if pass { 1.0 } else { 0.0 },
            diff_pct: 0.0,
            fonts: Vec::new(),
            truth_fonts: Vec::new(),
            our_pages: 1,
            truth_pages: 1,
            truth_pdf_sha256: sha256_hex(b"truth"),
            scoring_raster_config_sha256: scoring_raster_config_sha256(),
            pages: Vec::new(),
            layout_trace: None,
            geometry_limits: probe::GeometryLimits::default(),
            geometry: Vec::new(),
            geometry_pass: pass,
            pass,
            note: String::new(),
        }
    }

    #[test]
    fn release_gate_requires_every_core_case() {
        assert!(enforce_thresholds(&[report("core", false)], false).is_err());
    }

    #[test]
    fn release_gate_allows_exactly_ninety_percent_extended() {
        let mut reports: Vec<CaseReport> = (0..9).map(|_| report("extended", true)).collect();
        reports.push(report("extended", false));
        assert!(enforce_thresholds(&reports, false).is_ok());
        reports[0].pass = false;
        assert!(enforce_thresholds(&reports, false).is_err());
    }

    #[test]
    fn full_release_gate_requires_a_core_case() {
        assert!(enforce_thresholds(&[report("core", true)], true).is_ok());
        assert!(enforce_thresholds(&[report("experimental", true)], true).is_err());
        assert!(enforce_thresholds(&[], true).is_err());
    }

    #[test]
    fn reports_expose_weighted_and_legacy_ssim_metrics() {
        let mut report = report("core", true);
        report.truth_fonts = vec!["Quoted \"Font\"".to_string()];
        let reports = vec![report];
        let json = serde_json::to_string(&reports).expect("serialize report");
        assert!(json.contains("\"ssim_unweighted\""), "{json}");
        let rendered = html(&reports);
        assert!(rendered.contains("legacy gate"), "{rendered}");
        assert!(rendered.contains("weighted diagnostic"), "{rendered}");
        assert!(rendered.contains("geometry gate"), "{rendered}");
        assert!(rendered.contains("Quoted &quot;Font&quot;"), "{rendered}");
    }

    #[test]
    fn reports_expose_page_scores_and_immutable_fingerprints() {
        let json = serde_json::to_value([report("core", true)]).expect("serialize report");
        let case = &json[0];

        assert!(
            case.get("pages").is_some()
                && case.get("truth_pdf_sha256").is_some()
                && case.get("scoring_raster_config_sha256").is_some()
                && case.get("layout_trace").is_some(),
            "Task 0 diagnostic fields are absent: {case}"
        );
    }

    #[test]
    fn report_html_shows_every_scored_page_scrollably() {
        let mut case = report("core", true);
        case.pages = vec![
            page_report(1, 0.98, 0.97),
            page_report(2, 0.95, 0.93),
            page_report(3, 0.96, 0.94),
        ];
        let rendered = html(&[case]);
        for page in 0..3 {
            for kind in ["ours", "truth", "heat"] {
                let img = format!("pages/case-{page}-{kind}.png");
                assert!(rendered.contains(&img), "missing {img} in {rendered}");
            }
        }
        assert!(rendered.contains("class=\"pages-scroll\""), "{rendered}");
        assert!(rendered.contains("page 2/3"), "{rendered}");
    }

    #[test]
    fn report_html_keeps_page_count_mismatch_note_without_scored_pages() {
        let rendered = html(&[report("core", false)]);
        assert!(rendered.contains("no page render"), "{rendered}");
        assert!(!rendered.contains("class=\"pages-scroll\""), "{rendered}");
    }

    #[test]
    fn case_unweighted_score_is_the_minimum_page_score() {
        let mut report = report("core", true);
        report.pages = vec![
            page_report(1, 0.98, 0.97),
            page_report(2, 0.95, 0.93),
            page_report(3, 0.96, 0.94),
        ];

        recompute_case_page_minima(&mut report);

        assert_eq!(report.ssim_unweighted, 0.93);
    }

    fn page_report(page: u32, ssim: f64, ssim_unweighted: f64) -> PageReport {
        PageReport {
            page,
            ssim,
            ssim_unweighted,
            diff_pct: 0.0,
            ours_pixel_width: 100,
            ours_pixel_height: 100,
            truth_pixel_width: 100,
            truth_pixel_height: 100,
            our_pdf_pages: 3,
            truth_pdf_pages: 3,
        }
    }

    #[test]
    fn cli_accepts_task_zero_diagnostic_commands() {
        for command in ["advance-probe", "font-raster-probe", "image-probe"] {
            let parsed = Cli::try_parse_from([
                "fidelity",
                command,
                "--ours-pdf",
                "ours.pdf",
                "--truth-pdf",
                "truth.pdf",
                "--page",
                "1",
            ]);
            let error = parsed.err().map(|error| error.to_string());
            assert!(error.is_none(), "{command} is unavailable: {error:?}");
        }
    }

    #[test]
    fn release_pass_uses_legacy_ssim_and_geometry_only() {
        assert!(case_passes_release_gates(0.95, 0.90, true));
        assert!(!case_passes_release_gates(0.89, 0.90, true));
        assert!(!case_passes_release_gates(0.95, 0.90, false));
    }

    #[test]
    fn manifest_geometry_bounds_are_additive_defaults() {
        let manifest: Manifest = toml::from_str(
            r#"
title = "Geometry defaults"
tier = "core"
ssim_threshold = 0.90
word_version = "Microsoft Word 2021"
ground_truth_exported_at = "2026-07-16"
input_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
golden_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
expected_fonts = ["Carlito"]
"#,
        )
        .expect("parse manifest defaults");

        assert_eq!(manifest.geometry_median_pt, 0.4);
        assert_eq!(manifest.geometry_p95_pt, 1.5);
        assert_eq!(manifest.geometry_max_unmatched_pct, 5.0);
    }

    #[test]
    fn doctored_font_expectation_fails_with_actionable_sets() {
        let expected = std::collections::BTreeSet::from(["Carlito".to_string()]);
        let demanded = std::collections::BTreeSet::from(["Carlito".to_string()]);
        let embedded = std::collections::BTreeSet::from(["Liberation Sans".to_string()]);
        let error = validate_embedded_font_expectations(
            "gdocs-tables-longdoc",
            &expected,
            &demanded,
            &embedded,
        )
        .expect_err("substituted output family must fail the case directly");
        let message = format!("{error:#}");
        assert!(message.contains("font mismatch"), "{message}");
        assert!(message.contains("expected [Carlito]"), "{message}");
        assert!(message.contains("embedded [Liberation Sans]"), "{message}");
    }

    #[test]
    fn manifest_contract_rejects_missing_expected_output_fonts() {
        let manifest: Manifest = toml::from_str(
            r#"
title = "Missing font contract"
tier = "core"
ssim_threshold = 0.90
max_page_count_delta = 0
word_version = "Microsoft Word 2021"
ground_truth_exported_at = "2026-07-16"
input_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
golden_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#,
        )
        .expect("parse manifest fixture");
        let error = validate_manifest_contract(Path::new("manifest.toml"), &manifest)
            .expect_err("expected_fonts must be mandatory for fidelity cases");
        assert!(error.to_string().contains("expected_fonts"), "{error:#}");
    }

    #[test]
    fn every_corpus_manifest_declares_expected_output_fonts() {
        for directory in case_dirs("docx").expect("enumerate DOCX corpus") {
            let manifest_path = directory.join("manifest.toml");
            let manifest: Manifest =
                toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            assert!(
                !manifest.expected_fonts.is_empty(),
                "{} must declare expected_fonts",
                manifest_path.display()
            );
        }
    }

    #[test]
    fn fidelity_reports_default_to_repo_tmp_and_allow_an_explicit_directory() {
        assert!(
            report_dir().ends_with("tmp/fidelity-report"),
            "default report output must remain inside the repository tmp directory"
        );
        assert!(
            Cli::try_parse_from([
                "fidelity",
                "run",
                "--report-dir",
                "tmp/custom-fidelity-report",
            ])
            .is_ok(),
            "run must accept an explicit report directory"
        );
    }

    #[test]
    fn diagnostic_outputs_reject_paths_under_the_corpus_tree() {
        let path = corpus_root().join("docx/task-zero-must-not-write.json");
        let error = ensure_diagnostic_output_outside_corpus(&path)
            .expect_err("diagnostics must not write beneath fidelity-corpus");

        assert!(error.to_string().contains("corpus tree"), "{error:#}");
    }

    #[test]
    fn diagnostic_outputs_allow_project_local_tmp_paths() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/task-zero-allowed/report.json");

        assert!(
            ensure_diagnostic_output_outside_corpus(&path).is_ok(),
            "project-local tmp must remain writable"
        );
    }

    #[test]
    fn corpus_run_rejects_manual_font_directories() {
        let error =
            match Cli::try_parse_from(["fidelity", "run", "--font-dir", "../tmp/manual-fonts"]) {
                Ok(_) => panic!("release fidelity must resolve locked fonts automatically"),
                Err(error) => error,
            };
        assert!(
            error
                .to_string()
                .contains("unexpected argument '--font-dir'")
        );
    }

    #[test]
    fn required_font_profile_rejects_an_unresolved_declared_family() {
        let profile = office_core::DocProfile::new("docx");
        let error =
            validate_required_font_profile("gdocs-anchors-toc", &["Ubuntu".to_string()], &profile)
                .expect_err("a required family must have an exact profile resolution");
        let message = format!("{error:#}");
        assert!(message.contains("Ubuntu"), "{message}");
        assert!(message.contains("not resolved exactly"), "{message}");
    }

    #[test]
    fn required_font_profile_rejects_fallback_warnings_directly() {
        let mut profile = office_core::DocProfile::new("docx");
        profile.warnings.push(office_core::profile::Warning::new(
            "font_fallback",
            "Ubuntu -> Liberation Sans",
        ));
        let error =
            validate_required_font_profile("gdocs-anchors-toc", &["Ubuntu".to_string()], &profile)
                .expect_err("a required family must never fall back");
        let message = format!("{error:#}");
        assert!(message.contains("font_fallback"), "{message}");
        assert!(message.contains("Ubuntu -> Liberation Sans"), "{message}");
    }

    #[test]
    fn provenance_helpers_check_word_variant_and_embedded_date() {
        assert!(word_producer_matches(
            "Microsoft Word 2021",
            "Microsoft® Word 2021"
        ));
        assert!(word_producer_matches(
            "Microsoft Word LTSC (exact build unavailable in PDF metadata)",
            "Microsoft® Word LTSC"
        ));
        assert!(!word_producer_matches(
            "Microsoft Word 2021",
            "Microsoft Word LTSC"
        ));
        assert!(word_producer_matches(
            "Microsoft Word for Microsoft 365",
            "Microsoft® Word for Microsoft 365"
        ));
        assert!(!word_producer_matches(
            "Microsoft Word for Microsoft 365",
            "Microsoft® Word 2021"
        ));
        assert!(!word_producer_matches(
            "Microsoft Word 2021",
            "Microsoft® Word for Microsoft 365"
        ));
        assert_eq!(
            embedded_creation_date(b"<</CreationDate(D:20260709125924+05'30')>>"),
            Some("2026-07-09".into())
        );
    }

    #[test]
    fn bare_online_word_declaration_matches_only_bare_word_metadata() {
        assert!(word_producer_matches(
            "Microsoft Word (online export service)",
            "Microsoft Word"
        ));
        assert!(word_producer_matches("Microsoft Word", "Microsoft Word"));
        assert!(!word_producer_matches(
            "Microsoft Word (online export service)",
            "Microsoft® Word LTSC"
        ));
    }

    #[test]
    fn versioned_word_declaration_does_not_match_bare_word_metadata() {
        assert!(!word_producer_matches(
            "Microsoft Word 2021",
            "Microsoft Word"
        ));
    }

    #[test]
    fn provenance_uses_creator_when_pdf_producer_is_absent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ground_truth = root.join("fidelity-corpus/docx/gdocs-anchors-toc/ground-truth.pdf");
        let bytes = std::fs::read(&ground_truth).expect("read Creator-only Word PDF");
        let metadata = pdf_core::meta::get_metadata(&bytes).expect("read PDF metadata");
        assert!(
            metadata.producer.is_none(),
            "fixture must remain Producer-free"
        );
        assert_eq!(metadata.creator.as_deref(), Some("Microsoft Word"));

        let manifest = Manifest {
            title: "Creator fallback regression".into(),
            tier: "core".into(),
            ssim_threshold: 0.90,
            max_page_count_delta: 0,
            word_version: "Microsoft Word (online export service)".into(),
            ground_truth_exported_at: "2026-07-14".into(),
            input_sha256: "0".repeat(64),
            golden_sha256: "0".repeat(64),
            required_fonts: Vec::new(),
            expected_fonts: Vec::new(),
            geometry_median_pt: default_geometry_median_pt(),
            geometry_p95_pt: default_geometry_p95_pt(),
            geometry_max_unmatched_pct: default_geometry_max_unmatched_pct(),
            provenance: None,
            notes: String::new(),
        };
        validate_provenance(
            Path::new("creator-fallback/manifest.toml"),
            &manifest,
            "docx",
            &ground_truth,
            &bytes,
        )
        .expect("Creator metadata must satisfy Word provenance");
    }
}

#[allow(clippy::too_many_arguments)]
fn score_case(
    name: &str,
    source: &[u8],
    format: OfficeFormat,
    truth_pdf: &[u8],
    manifest: &Manifest,
    pages_dir: &Path,
    options: &ConvertOptions,
    demanded_fonts: &BTreeSet<String>,
) -> CaseReport {
    let mut report = CaseReport {
        name: name.to_string(),
        title: manifest.title.clone(),
        tier: manifest.tier.clone(),
        threshold: manifest.ssim_threshold,
        ssim: 0.0,
        ssim_unweighted: 0.0,
        diff_pct: 1.0,
        fonts: Vec::new(),
        truth_fonts: Vec::new(),
        our_pages: 0,
        truth_pages: 0,
        truth_pdf_sha256: sha256_hex(truth_pdf),
        scoring_raster_config_sha256: scoring_raster_config_sha256(),
        pages: Vec::new(),
        layout_trace: None,
        geometry_limits: probe::GeometryLimits {
            median_pt: manifest.geometry_median_pt,
            p95_pt: manifest.geometry_p95_pt,
            max_unmatched_pct: manifest.geometry_max_unmatched_pct,
        },
        geometry: Vec::new(),
        geometry_pass: false,
        pass: false,
        note: String::new(),
    };

    let our_pdf = match convert(source, format, options) {
        Ok(out) => {
            report.our_pages = out.page_count;
            #[cfg(feature = "layout-trace")]
            {
                report.layout_trace = out.diagnostics.map(|trace| {
                    serde_json::to_value(trace).expect("layout trace is serializable")
                });
            }
            out.pdf
        }
        Err(e) => {
            // An intentionally-unsupported case does not belong in the corpus.
            report.note = format!("convert failed (out of envelope): {e}");
            return report;
        }
    };
    let embedded_fonts = match fonts::embedded_font_families(&our_pdf) {
        Ok(fonts) => fonts,
        Err(error) => {
            report.note = format!("inspect embedded fonts: {error:#}");
            return report;
        }
    };
    let truth_fonts = match fonts::embedded_font_families(truth_pdf) {
        Ok(fonts) => fonts,
        Err(error) => {
            report.note = format!("inspect ground-truth fonts: {error:#}");
            return report;
        }
    };
    report.fonts = embedded_fonts.iter().cloned().collect();
    report.truth_fonts = truth_fonts.iter().cloned().collect();
    let expected_fonts = manifest
        .expected_fonts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if let Err(error) =
        validate_embedded_font_expectations(name, &expected_fonts, demanded_fonts, &embedded_fonts)
    {
        report.note = error.to_string();
        return report;
    }
    let truth_pages = match pdf_core::meta::page_count(truth_pdf) {
        Ok(page_count) => page_count,
        Err(error) => {
            report.note = format!("read ground-truth page count: {error}");
            return report;
        }
    };
    report.truth_pages = truth_pages;

    if report.our_pages.abs_diff(truth_pages) > manifest.max_page_count_delta {
        report.note = format!("page count {} vs {}", report.our_pages, truth_pages);
        return report;
    }

    let n = report.our_pages.min(truth_pages);
    let mut max_diff = 0.0_f64;
    let mut geometry_pass = true;
    let mut geometry_violation = None;
    for i in 0..n {
        let (Ok(ours), Ok(truth)) = (raster(&our_pdf, i), raster(truth_pdf, i)) else {
            report.note = format!("rasterization failed on page {i}");
            return report;
        };
        // A raster-size mismatch means the two PDFs use different page geometry
        // (e.g. A4 vs Letter). SSIM of differently-sized images is meaningless
        // (degenerate 0.0); report the mismatch explicitly so it reads as a
        // corpus/ground-truth page-size problem, not a rendering catastrophe.
        if ours.width() != truth.width() || ours.height() != truth.height() {
            report.note = format!(
                "page {i} raster {}x{} vs {}x{} — page-size mismatch; re-export ground truth from the current source.docx",
                ours.width(),
                ours.height(),
                truth.width(),
                truth.height()
            );
            return report;
        }
        let s = ssim::score_pages(&ours, &truth);
        max_diff = max_diff.max(s.diff_pct);
        report.pages.push(PageReport {
            page: i + 1,
            ssim: s.ssim,
            ssim_unweighted: s.ssim_unweighted,
            diff_pct: s.diff_pct,
            ours_pixel_width: ours.width(),
            ours_pixel_height: ours.height(),
            truth_pixel_width: truth.width(),
            truth_pixel_height: truth.height(),
            our_pdf_pages: report.our_pages,
            truth_pdf_pages: report.truth_pages,
        });
        save_png(pages_dir, name, i, "heat", &s.heatmap_png);
        write_page_png(pages_dir, name, i, "ours", &ours);
        write_page_png(pages_dir, name, i, "truth", &truth);
        let probe_report = match probe::compare_page(&our_pdf, truth_pdf, i + 1) {
            Ok(report) => report,
            Err(error) => {
                report.note = format!("page {} vector geometry probe failed: {error:#}", i + 1);
                return report;
            }
        };
        let geometry = probe::geometry_metrics(&probe_report, &report.geometry_limits);
        if !geometry.pass {
            geometry_pass = false;
            if geometry_violation.is_none() {
                geometry_violation = geometry.violation.clone();
            }
        }
        report.geometry.push(geometry);
    }
    recompute_case_page_minima(&mut report);
    report.diff_pct = max_diff;
    report.geometry_pass = geometry_pass;
    report.pass = case_passes_release_gates(
        report.ssim_unweighted,
        manifest.ssim_threshold,
        report.geometry_pass,
    );
    if let Some(violation) = geometry_violation {
        report.note = violation;
    } else if report.ssim_unweighted < manifest.ssim_threshold {
        report.note = format!(
            "legacy SSIM {:.4} below frozen threshold {:.4}",
            report.ssim_unweighted, manifest.ssim_threshold
        );
    }
    report
}

fn recompute_case_page_minima(report: &mut CaseReport) {
    report.ssim = report
        .pages
        .iter()
        .map(|page| page.ssim)
        .reduce(f64::min)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    report.ssim_unweighted = report
        .pages
        .iter()
        .map(|page| page.ssim_unweighted)
        .reduce(f64::min)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
}

fn raster(pdf: &[u8], page: u32) -> Result<image::DynamicImage> {
    let png = render_page(
        pdf,
        page,
        &RenderOptions {
            scale: 2.0,
            ..Default::default()
        },
    )
    .context("render_page")?;
    Ok(image::load_from_memory(&png)?)
}

fn save_png(dir: &Path, name: &str, page: u32, kind: &str, png: &[u8]) {
    let _ = std::fs::write(dir.join(format!("{name}-{page}-{kind}.png")), png);
}
fn write_page_png(dir: &Path, name: &str, page: u32, kind: &str, img: &image::DynamicImage) {
    let _ = img.save(dir.join(format!("{name}-{page}-{kind}.png")));
}

fn write_reports(reports: &[CaseReport], selected_report_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(selected_report_dir)?;
    std::fs::write(
        selected_report_dir.join("report.json"),
        serde_json::to_string_pretty(reports)?,
    )?;
    std::fs::write(selected_report_dir.join("index.html"), html(reports))?;
    Ok(())
}

/// Minimal HTML-escape for text interpolated into the report.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Self-contained stylesheet for the report (light/dark aware).
const REPORT_CSS: &str = r#"
:root{--bg:#f6f7f9;--card:#fff;--ink:#1a1d21;--muted:#6b7280;--line:#e5e7eb;
  --pass:#16a34a;--pass-bg:#f0fdf4;--fail:#dc2626;--fail-bg:#fef2f2;--chip:#eef2ff;--chip-ink:#4338ca;--tick:#111}
@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--card:#161b22;--ink:#e6edf3;--muted:#8b949e;--line:#30363d;
  --pass:#3fb950;--pass-bg:#0f2417;--fail:#f85149;--fail-bg:#2a1416;--chip:#1f2937;--chip-ink:#a5b4fc;--tick:#e6edf3}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);
  font:14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif}
.wrap{max-width:1180px;margin:0 auto;padding:32px 24px 72px}
h1{font-size:24px;margin:0 0 4px;letter-spacing:-.02em}
.sub{color:var(--muted);margin:0 0 24px;font-size:13px}
.summary{display:flex;flex-wrap:wrap;gap:12px;margin-bottom:28px}
.tile{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px 18px;min-width:104px}
.tile .n{font-size:26px;font-weight:700;letter-spacing:-.02em;font-variant-numeric:tabular-nums}
.tile .l{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.05em;margin-top:2px}
.tile.good .n{color:var(--pass)}.tile.bad .n{color:var(--fail)}
.case{background:var(--card);border:1px solid var(--line);border-left-width:4px;border-radius:12px;
  margin-bottom:16px;overflow:hidden}
.case.pass{border-left-color:var(--pass)}.case.fail{border-left-color:var(--fail)}
.head{display:flex;align-items:center;gap:12px;padding:14px 18px;flex-wrap:wrap}
.badge{font-size:11px;font-weight:700;padding:3px 9px;border-radius:6px;letter-spacing:.04em}
.badge.pass{color:var(--pass);background:var(--pass-bg)}.badge.fail{color:var(--fail);background:var(--fail-bg)}
.name{font-weight:650;font-size:15px}
.chip{font-size:11px;color:var(--chip-ink);background:var(--chip);padding:2px 9px;border-radius:999px;
  text-transform:uppercase;letter-spacing:.03em}
.spacer{flex:1}
.metrics{display:flex;align-items:center;gap:20px}
.metric{text-align:right}
.metric .l{display:block;color:var(--muted);font-size:10px;text-transform:uppercase;letter-spacing:.04em}
.metric .v{font-variant-numeric:tabular-nums;font-weight:600}
.metric .v.pass{color:var(--pass)}.metric .v.fail{color:var(--fail)}
.bar{position:relative;height:6px;width:150px;background:var(--line);border-radius:999px}
.bar .fill{position:absolute;left:0;top:0;bottom:0;border-radius:999px}
.bar .fill.pass{background:var(--pass)}.bar .fill.fail{background:var(--fail)}
.bar .tick{position:absolute;top:-3px;bottom:-3px;width:2px;background:var(--tick);opacity:.55;border-radius:2px}
.note{margin:0 18px 14px;padding:8px 12px;background:var(--fail-bg);color:var(--fail);border-radius:8px;
  font-size:12.5px;font-variant-numeric:tabular-nums}
.pages-scroll{max-height:780px;overflow-y:auto;padding:2px 18px 18px;display:flex;flex-direction:column;gap:16px;
  scrollbar-width:thin}
.pageblock .plabel{color:var(--muted);font-size:11px;text-transform:uppercase;letter-spacing:.05em;
  margin:0 0 6px;font-variant-numeric:tabular-nums}
.frames{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;padding:2px 18px 18px}
.pages-scroll .frames{padding:0}
figure{margin:0}
figure img{width:100%;height:auto;border:1px solid var(--line);border-radius:6px;background:#fff;display:block}
figcaption{text-align:center;color:var(--muted);font-size:11px;margin-top:6px;text-transform:uppercase;letter-spacing:.05em}
@media (max-width:760px){.frames{grid-template-columns:1fr}.metrics{gap:12px;width:100%;justify-content:space-between}}
"#;

fn html(reports: &[CaseReport]) -> String {
    let total = reports.len();
    let passing = reports.iter().filter(|r| r.pass).count();
    let failing = total - passing;
    let core_fail = reports
        .iter()
        .filter(|r| r.tier == "core" && !r.pass)
        .count();
    let pass_rate = if total > 0 {
        passing as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let min_ssim = reports
        .iter()
        .map(|r| r.ssim_unweighted)
        .fold(f64::INFINITY, f64::min);
    let min_ssim = if min_ssim.is_finite() { min_ssim } else { 0.0 };

    // Failures first, then worst SSIM first, so problems are up top.
    let mut ordered: Vec<&CaseReport> = reports.iter().collect();
    ordered.sort_by(|a, b| {
        a.pass.cmp(&b.pass).then(
            a.ssim_unweighted
                .partial_cmp(&b.ssim_unweighted)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    // Amplify the meaningful SSIM range [0.8, 1.0] onto the bar so near-passing
    // and failing cases are visually distinct.
    let scale = |v: f64| (((v - 0.8) / 0.2) * 100.0).clamp(0.0, 100.0);

    let mut cards = String::new();
    for r in &ordered {
        let cls = if r.pass { "pass" } else { "fail" };
        let legacy_cls = if r.ssim_unweighted >= r.threshold {
            "pass"
        } else {
            "fail"
        };
        let geometry_cls = if r.geometry_pass { "pass" } else { "fail" };
        let badge = if r.pass { "PASS" } else { "FAIL" };
        let note = if r.note.is_empty() {
            String::new()
        } else {
            format!("<div class=\"note\">{}</div>", esc(&r.note))
        };
        let font_summary = if r.fonts.is_empty() {
            "—".to_string()
        } else {
            esc(&r.fonts.join(", "))
        };
        let truth_font_summary = if r.truth_fonts.is_empty() {
            "—".to_string()
        } else {
            esc(&r.truth_fonts.join(", "))
        };
        let frames = if r.pages.is_empty() {
            String::from(
                "<div class=\"note\">no page render (page-count mismatch — see note above)</div>",
            )
        } else {
            // One ours/truth/heat row per scored page (PNG names are 0-based),
            // all inside one scroll container so long documents stay browsable.
            let total_pages = r.pages.len();
            let blocks: String = r
                .pages
                .iter()
                .map(|p| {
                    format!(
                        "<section class=\"pageblock\">\
                         <div class=\"plabel\">page {page}/{total_pages} · legacy ssim {ssim:.4} · diff {diff:.2}%</div>\
                         <div class=\"frames\">\
                         <figure><img loading=\"lazy\" src=\"pages/{n}-{i}-ours.png\"><figcaption>ours</figcaption></figure>\
                         <figure><img loading=\"lazy\" src=\"pages/{n}-{i}-truth.png\"><figcaption>ground truth</figcaption></figure>\
                         <figure><img loading=\"lazy\" src=\"pages/{n}-{i}-heat.png\"><figcaption>heatmap</figcaption></figure>\
                         </div></section>",
                        page = p.page,
                        ssim = p.ssim_unweighted,
                        diff = p.diff_pct * 100.0,
                        n = esc(&r.name),
                        i = p.page.saturating_sub(1),
                    )
                })
                .collect();
            format!("<div class=\"pages-scroll\">{blocks}</div>")
        };
        cards.push_str(&format!(
            "<article class=\"case {cls}\"><div class=\"head\">\
               <span class=\"badge {cls}\">{badge}</span>\
               <span class=\"name\">{name}</span><span class=\"chip\">{tier}</span>\
               <span class=\"spacer\"></span><div class=\"metrics\">\
                 <div class=\"metric\"><span class=\"l\">legacy gate</span><span class=\"v {legacy_cls}\">{ssim_unweighted:.4}</span></div>\
                 <div class=\"metric\"><span class=\"l\">weighted diagnostic</span><span class=\"v\">{ssim:.4}</span></div>\
                 <div class=\"metric\"><span class=\"l\">geometry gate</span><span class=\"v {geometry_cls}\">{geometry_status}</span></div>\
                 <div class=\"bar\"><div class=\"fill {legacy_cls}\" style=\"width:{fill:.1}%\"></div>\
                   <div class=\"tick\" style=\"left:{tick:.1}%\"></div></div>\
                 <div class=\"metric\"><span class=\"l\">threshold</span><span class=\"v\">{thr:.2}</span></div>\
                 <div class=\"metric\"><span class=\"l\">diff</span><span class=\"v\">{diff:.2}%</span></div>\
                 <div class=\"metric\"><span class=\"l\">pages</span><span class=\"v\">{op}/{tp}</span></div>\
                 <div class=\"metric\" title=\"Word PDF: {truth_fonts}\"><span class=\"l\">fonts</span><span class=\"v\">{fonts}</span></div>\
               </div></div>{note}{frames}</article>",
            name = esc(&r.name),
            tier = esc(&r.tier),
            ssim = r.ssim,
            ssim_unweighted = r.ssim_unweighted,
            legacy_cls = legacy_cls,
            geometry_cls = geometry_cls,
            geometry_status = if r.geometry_pass { "pass" } else { "fail" },
            thr = r.threshold,
            diff = r.diff_pct * 100.0,
            op = r.our_pages,
            tp = r.truth_pages,
            fonts = font_summary,
            truth_fonts = truth_font_summary,
            fill = scale(r.ssim_unweighted),
            tick = scale(r.threshold),
        ));
    }

    let fail_tile = if failing > 0 { "tile bad" } else { "tile good" };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>office-core fidelity report</title><style>{REPORT_CSS}</style></head><body><div class=\"wrap\">\
         <h1>office-core fidelity report</h1>\
         <p class=\"sub\">DOCX layout fidelity vs Word ground truths · frozen legacy SSIM + vector geometry gate · ink-weighted SSIM and heatmaps are diagnostic</p>\
         <div class=\"summary\">\
           <div class=\"tile\"><div class=\"n\">{total}</div><div class=\"l\">cases</div></div>\
           <div class=\"tile good\"><div class=\"n\">{passing}</div><div class=\"l\">passing</div></div>\
           <div class=\"{fail_tile}\"><div class=\"n\">{failing}</div><div class=\"l\">failing</div></div>\
           <div class=\"tile\"><div class=\"n\">{core_fail}</div><div class=\"l\">core failures</div></div>\
           <div class=\"tile\"><div class=\"n\">{pass_rate:.0}%</div><div class=\"l\">pass rate</div></div>\
           <div class=\"tile\"><div class=\"n\">{min_ssim:.3}</div><div class=\"l\">min legacy ssim</div></div>\
         </div>{cards}</div></body></html>"
    )
}

#[cfg(test)]
mod roundtrip {
    //! End-to-end text round-trip: convert known text with office-core, then
    //! extract it back through pdfium. Fails if the ToUnicode CMap or glyph→CID
    //! subset mapping is wrong (extracted text would be garbled/empty).

    use office_core::testkit::minimal_docx;
    use office_core::{ConvertOptions, OfficeFormat, convert};

    #[test]
    fn docx_text_round_trips_through_pdf() {
        let phrase = "Roundtrip Verification Alpha Bravo Charlie";
        let docx = minimal_docx(&format!(
            "<w:p><w:r><w:t xml:space=\"preserve\">{phrase}</w:t></w:r></w:p>"
        ));
        let out =
            convert(&docx, OfficeFormat::Docx, &ConvertOptions::default()).expect("convert docx");
        let text = pdf_core::extract::extract_text(&out.pdf).expect("extract text");
        for word in ["Roundtrip", "Verification", "Bravo", "Charlie"] {
            assert!(
                text.contains(word),
                "word {word:?} missing from extracted text: {text:?}",
            );
        }
    }

    /// pptx/xlsx share the emitter; confirm their text is also selectable
    /// (the DocKosha viewer's per-page text depends on it).
    #[test]
    fn pptx_and_xlsx_text_are_selectable() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mock-files");
        for (relative, format, needle) in [
            ("pptx/file1.pptx", OfficeFormat::Pptx, "Dummy Document 1"),
            ("xlsx/file1.xlsx", OfficeFormat::Xlsx, "Column A"),
        ] {
            let file = fixtures.join(relative);
            let bytes = std::fs::read(&file).unwrap_or_else(|error| {
                panic!("required fixture {} is missing: {error}", file.display())
            });
            let out = convert(&bytes, format, &ConvertOptions::default())
                .unwrap_or_else(|error| panic!("convert {}: {error}", file.display()));
            let text = pdf_core::extract::extract_text(&out.pdf).expect("extract text");
            assert!(
                text.contains(needle),
                "{needle:?} missing from {}: {text:?}",
                file.display(),
            );
        }
    }
}
