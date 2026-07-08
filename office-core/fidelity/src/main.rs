//! The fidelity harness runner: seed the corpus, score cases against Word
//! ground truths, and emit JSON + HTML reports.
//!
//! Ground-truth PDFs are produced by a human (task H3); `run` scores only the
//! cases that have one and `missing-ground-truth` lists the rest.

mod corpus;
mod ssim;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use office_core::{ConvertOptions, OfficeFormat, convert};
use pdf_core::raster::{RenderOptions, render_page};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fidelity-corpus")
}
fn report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fidelity-report")
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
    /// Score cases that have a ground truth; exit 1 if any core case fails.
    Run {
        #[arg(long, default_value = "docx")]
        format: String,
        #[arg(long)]
        case: Option<String>,
        #[arg(long)]
        update_report: bool,
    },
    /// List corpus cases lacking a `ground-truth.pdf`.
    MissingGroundTruth,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    title: String,
    #[serde(default = "default_tier")]
    tier: String,
    #[serde(default = "default_threshold")]
    ssim_threshold: f64,
    #[serde(default)]
    max_page_count_delta: u32,
}
fn default_tier() -> String {
    "extended".into()
}
fn default_threshold() -> f64 {
    0.97
}

#[derive(Serialize)]
struct CaseReport {
    name: String,
    title: String,
    tier: String,
    threshold: f64,
    ssim: f64,
    diff_pct: f64,
    our_pages: u32,
    truth_pages: u32,
    pass: bool,
    note: String,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Seed => {
            let n = corpus::seed(&corpus_root())?;
            println!("seeded {n} docx cases under {}", corpus_root().display());
        }
        Cmd::MissingGroundTruth => list_missing()?,
        Cmd::Run {
            format,
            case,
            update_report,
        } => run(&format, case.as_deref(), update_report)?,
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
    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn run(format: &str, only: Option<&str>, _update_report: bool) -> Result<()> {
    let pages_dir = report_dir().join("pages");
    std::fs::create_dir_all(&pages_dir).ok();

    let mut reports = Vec::new();
    let mut scored = 0;
    for dir in case_dirs(format)? {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        if let Some(only) = only
            && only != name
        {
            continue;
        }
        let gt = dir.join("ground-truth.pdf");
        if !gt.exists() {
            continue; // reported by missing-ground-truth
        }
        scored += 1;
        let manifest: Manifest =
            toml::from_str(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap_or_default())
                .unwrap_or_else(|_| Manifest {
                    title: name.clone(),
                    tier: default_tier(),
                    ssim_threshold: default_threshold(),
                    max_page_count_delta: 0,
                });
        reports.push(score_case(&name, &dir, &gt, &manifest, &pages_dir));
    }

    if scored == 0 {
        println!(
            "no ground truths present yet — run `fidelity seed` then export Word PDFs (human task H3)."
        );
        return Ok(());
    }

    write_reports(&reports)?;
    let failed_core: Vec<&CaseReport> = reports
        .iter()
        .filter(|r| r.tier == "core" && !r.pass)
        .collect();
    println!(
        "scored {} cases; {} core failures. report: {}",
        reports.len(),
        failed_core.len(),
        report_dir().join("index.html").display()
    );
    if !failed_core.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

fn score_case(
    name: &str,
    dir: &Path,
    gt: &Path,
    manifest: &Manifest,
    pages_dir: &Path,
) -> CaseReport {
    let mut report = CaseReport {
        name: name.to_string(),
        title: manifest.title.clone(),
        tier: manifest.tier.clone(),
        threshold: manifest.ssim_threshold,
        ssim: 0.0,
        diff_pct: 1.0,
        our_pages: 0,
        truth_pages: 0,
        pass: false,
        note: String::new(),
    };

    let source = match std::fs::read(dir.join("source.docx")) {
        Ok(s) => s,
        Err(e) => {
            report.note = format!("read source: {e}");
            return report;
        }
    };
    let our_pdf = match convert(&source, OfficeFormat::Docx, &ConvertOptions::default()) {
        Ok(out) => {
            report.our_pages = out.page_count;
            out.pdf
        }
        Err(e) => {
            // An intentionally-unsupported case does not belong in the corpus.
            report.note = format!("convert failed (out of envelope): {e}");
            return report;
        }
    };
    let truth_pdf = std::fs::read(gt).unwrap_or_default();
    let truth_pages = probe_page_count(&truth_pdf);
    report.truth_pages = truth_pages;

    if report.our_pages.abs_diff(truth_pages) > manifest.max_page_count_delta {
        report.note = format!("page count {} vs {}", report.our_pages, truth_pages);
        return report;
    }

    let n = report.our_pages.min(truth_pages);
    let mut min_ssim = f64::INFINITY;
    let mut max_diff = 0.0_f64;
    for i in 0..n {
        let (Ok(ours), Ok(truth)) = (raster(&our_pdf, i), raster(&truth_pdf, i)) else {
            report.note = format!("rasterization failed on page {i}");
            return report;
        };
        let s = ssim::score_pages(&ours, &truth);
        min_ssim = min_ssim.min(s.ssim);
        max_diff = max_diff.max(s.diff_pct);
        save_png(pages_dir, name, i, "heat", &s.heatmap_png);
        write_page_png(pages_dir, name, i, "ours", &ours);
        write_page_png(pages_dir, name, i, "truth", &truth);
    }
    report.ssim = if min_ssim.is_finite() { min_ssim } else { 0.0 };
    report.diff_pct = max_diff;
    report.pass = report.ssim >= manifest.ssim_threshold;
    report
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

/// Counts pages by rendering until an out-of-range error.
fn probe_page_count(pdf: &[u8]) -> u32 {
    let mut n = 0;
    while n < 10_000
        && render_page(
            pdf,
            n,
            &RenderOptions {
                scale: 0.2,
                ..Default::default()
            },
        )
        .is_ok()
    {
        n += 1;
    }
    n
}

fn save_png(dir: &Path, name: &str, page: u32, kind: &str, png: &[u8]) {
    let _ = std::fs::write(dir.join(format!("{name}-{page}-{kind}.png")), png);
}
fn write_page_png(dir: &Path, name: &str, page: u32, kind: &str, img: &image::DynamicImage) {
    let _ = img.save(dir.join(format!("{name}-{page}-{kind}.png")));
}

fn write_reports(reports: &[CaseReport]) -> Result<()> {
    std::fs::create_dir_all(report_dir())?;
    std::fs::write(
        report_dir().join("report.json"),
        serde_json::to_string_pretty(reports)?,
    )?;
    std::fs::write(report_dir().join("index.html"), html(reports))?;
    Ok(())
}

fn html(reports: &[CaseReport]) -> String {
    let mut rows = String::new();
    for r in reports {
        let badge = if r.pass { "✅" } else { "❌" };
        rows.push_str(&format!(
            "<tr><td>{badge}</td><td>{}</td><td>{}</td><td>{:.4}</td><td>{:.2}%</td><td>{}/{}</td></tr>\n\
             <tr><td colspan=6><img src='pages/{}-0-ours.png' width=260> <img src='pages/{}-0-truth.png' width=260> <img src='pages/{}-0-heat.png' width=260></td></tr>",
            r.name, r.tier, r.ssim, r.diff_pct * 100.0, r.our_pages, r.truth_pages, r.name, r.name, r.name
        ));
    }
    format!(
        "<!doctype html><meta charset=utf-8><title>office-core fidelity</title>\
        <style>body{{font:14px system-ui;margin:2rem}}table{{border-collapse:collapse;width:100%}}\
        td,th{{border:1px solid #ddd;padding:6px;text-align:left}}img{{border:1px solid #ccc;margin:2px}}</style>\
        <h1>office-core fidelity report</h1>\
        <p>ours | ground truth | heatmap (red = SSIM &lt; 0.9)</p>\
        <table><tr><th></th><th>case</th><th>tier</th><th>SSIM</th><th>diff</th><th>pages</th></tr>{rows}</table>"
    )
}
