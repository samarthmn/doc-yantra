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
        for (file, format, needle) in [
            (
                "../mock-files/pptx/file1.pptx",
                OfficeFormat::Pptx,
                "Dummy Document 1",
            ),
            (
                "../mock-files/xlsx/file1.xlsx",
                OfficeFormat::Xlsx,
                "Column A",
            ),
        ] {
            let Ok(bytes) = std::fs::read(file) else {
                continue;
            };
            let out = convert(&bytes, format, &ConvertOptions::default())
                .unwrap_or_else(|e| panic!("convert {file}: {e}"));
            let text = pdf_core::extract::extract_text(&out.pdf).expect("extract text");
            assert!(
                text.contains(needle),
                "{needle:?} missing from {file}: {text:?}",
            );
        }
    }
}
