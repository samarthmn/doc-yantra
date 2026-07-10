//! The fidelity harness runner: seed the corpus, score cases against Word
//! ground truths, and emit JSON + HTML reports.
//!
//! Ground-truth PDFs are produced by a human. `run` is strict: every selected
//! case needs a source, manifest, ground truth, and recorded Word provenance.

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
    /// Score the complete reference corpus and enforce its release thresholds.
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
    title: String,
    tier: String,
    ssim_threshold: f64,
    #[serde(default)]
    max_page_count_delta: u32,
    word_version: String,
    ground_truth_exported_at: String,
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

fn run(format: &str, only: Option<&str>, _update_report: bool) -> Result<()> {
    if format != "docx" {
        anyhow::bail!(
            "no {format} reference corpus is committed; refusing to claim fidelity coverage"
        );
    }
    let pages_dir = report_dir().join("pages");
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
        let source = dir.join("source.docx");
        if !source.is_file() {
            anyhow::bail!("selected case {name} is missing {}", source.display());
        }
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
        validate_provenance(&manifest_path, &manifest, &gt)?;
        reports.push(score_case(&name, &dir, &gt, &manifest, &pages_dir));
    }

    write_reports(&reports)?;
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
    println!(
        "scored {} cases; {} core failures; {}/{} extended passed. report: {}",
        reports.len(),
        core_failures,
        extended_passes,
        extended_total,
        report_dir().join("index.html").display()
    );
    Ok(())
}

fn validate_manifest_contract(path: &Path, manifest: &Manifest) -> Result<()> {
    if manifest.title.trim().is_empty() {
        anyhow::bail!("{} has an empty title", path.display());
    }
    if manifest.tier != "core" && manifest.tier != "extended" {
        anyhow::bail!("{} has an unknown fidelity tier", path.display());
    }
    if !manifest.ssim_threshold.is_finite() || !(0.90..=1.0).contains(&manifest.ssim_threshold) {
        anyhow::bail!(
            "{} must use an SSIM threshold between 0.90 and 1.0",
            path.display()
        );
    }
    if manifest.max_page_count_delta > 1 {
        anyhow::bail!("{} allows an excessive page-count delta", path.display());
    }
    Ok(())
}

fn validate_provenance(path: &Path, manifest: &Manifest, ground_truth: &Path) -> Result<()> {
    let declared_producer = manifest.word_version.trim();
    if !declared_producer
        .to_ascii_lowercase()
        .starts_with("microsoft word")
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

    let pdf =
        std::fs::read(ground_truth).with_context(|| format!("read {}", ground_truth.display()))?;
    let metadata = pdf_core::meta::get_metadata(&pdf)
        .with_context(|| format!("read PDF metadata from {}", ground_truth.display()))?;
    let actual_producer = metadata.producer.or(metadata.creator).ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no PDF Producer or Creator metadata",
            ground_truth.display()
        )
    })?;
    if !word_producer_matches(declared_producer, &actual_producer) {
        anyhow::bail!(
            "{} declares {declared_producer:?}, but PDF metadata reports {actual_producer:?}",
            path.display()
        );
    }
    let actual_date = embedded_creation_date(&pdf).ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no parseable PDF CreationDate",
            ground_truth.display()
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
    if reports
        .iter()
        .any(|report| report.tier != "core" && report.tier != "extended")
    {
        anyhow::bail!("fidelity manifest has an unknown tier");
    }
    if require_both_tiers && (core.is_empty() || extended.is_empty()) {
        anyhow::bail!("full fidelity corpus must contain both core and extended cases");
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
            diff_pct: 0.0,
            our_pages: 1,
            truth_pages: 1,
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
    fn full_release_gate_requires_both_tiers() {
        assert!(enforce_thresholds(&[report("core", true)], true).is_err());
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

/// Minimal HTML-escape for text interpolated into the report.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
.frames{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;padding:2px 18px 18px}
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
    let min_ssim = reports.iter().map(|r| r.ssim).fold(f64::INFINITY, f64::min);
    let min_ssim = if min_ssim.is_finite() { min_ssim } else { 0.0 };

    // Failures first, then worst SSIM first, so problems are up top.
    let mut ordered: Vec<&CaseReport> = reports.iter().collect();
    ordered.sort_by(|a, b| {
        a.pass.cmp(&b.pass).then(
            a.ssim
                .partial_cmp(&b.ssim)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    // Amplify the meaningful SSIM range [0.8, 1.0] onto the bar so near-passing
    // and failing cases are visually distinct.
    let scale = |v: f64| (((v - 0.8) / 0.2) * 100.0).clamp(0.0, 100.0);
    let pages = report_dir().join("pages");

    let mut cards = String::new();
    for r in &ordered {
        let cls = if r.pass { "pass" } else { "fail" };
        let badge = if r.pass { "PASS" } else { "FAIL" };
        let note = if r.note.is_empty() {
            String::new()
        } else {
            format!("<div class=\"note\">{}</div>", esc(&r.note))
        };
        let frames = if pages.join(format!("{}-0-ours.png", r.name)).exists() {
            format!(
                "<div class=\"frames\">\
                 <figure><img loading=\"lazy\" src=\"pages/{n}-0-ours.png\"><figcaption>ours</figcaption></figure>\
                 <figure><img loading=\"lazy\" src=\"pages/{n}-0-truth.png\"><figcaption>ground truth</figcaption></figure>\
                 <figure><img loading=\"lazy\" src=\"pages/{n}-0-heat.png\"><figcaption>heatmap</figcaption></figure>\
                 </div>",
                n = esc(&r.name)
            )
        } else {
            String::from(
                "<div class=\"note\">no page render (page-count mismatch — see note above)</div>",
            )
        };
        cards.push_str(&format!(
            "<article class=\"case {cls}\"><div class=\"head\">\
               <span class=\"badge {cls}\">{badge}</span>\
               <span class=\"name\">{name}</span><span class=\"chip\">{tier}</span>\
               <span class=\"spacer\"></span><div class=\"metrics\">\
                 <div class=\"metric\"><span class=\"l\">ssim</span><span class=\"v {cls}\">{ssim:.4}</span></div>\
                 <div class=\"bar\"><div class=\"fill {cls}\" style=\"width:{fill:.1}%\"></div>\
                   <div class=\"tick\" style=\"left:{tick:.1}%\"></div></div>\
                 <div class=\"metric\"><span class=\"l\">threshold</span><span class=\"v\">{thr:.2}</span></div>\
                 <div class=\"metric\"><span class=\"l\">diff</span><span class=\"v\">{diff:.2}%</span></div>\
                 <div class=\"metric\"><span class=\"l\">pages</span><span class=\"v\">{op}/{tp}</span></div>\
               </div></div>{note}{frames}</article>",
            name = esc(&r.name),
            tier = esc(&r.tier),
            ssim = r.ssim,
            thr = r.threshold,
            diff = r.diff_pct * 100.0,
            op = r.our_pages,
            tp = r.truth_pages,
            fill = scale(r.ssim),
            tick = scale(r.threshold),
        ));
    }

    let fail_tile = if failing > 0 { "tile bad" } else { "tile good" };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>office-core fidelity report</title><style>{REPORT_CSS}</style></head><body><div class=\"wrap\">\
         <h1>office-core fidelity report</h1>\
         <p class=\"sub\">DOCX layout fidelity vs Word ground truths · ours | ground truth | heatmap (red = SSIM &lt; 0.9)</p>\
         <div class=\"summary\">\
           <div class=\"tile\"><div class=\"n\">{total}</div><div class=\"l\">cases</div></div>\
           <div class=\"tile good\"><div class=\"n\">{passing}</div><div class=\"l\">passing</div></div>\
           <div class=\"{fail_tile}\"><div class=\"n\">{failing}</div><div class=\"l\">failing</div></div>\
           <div class=\"tile\"><div class=\"n\">{core_fail}</div><div class=\"l\">core failures</div></div>\
           <div class=\"tile\"><div class=\"n\">{pass_rate:.0}%</div><div class=\"l\">pass rate</div></div>\
           <div class=\"tile\"><div class=\"n\">{min_ssim:.3}</div><div class=\"l\">min ssim</div></div>\
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
