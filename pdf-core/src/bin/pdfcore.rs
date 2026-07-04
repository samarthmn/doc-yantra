//! pdfcore — CLI over the pdf-core engine, for smoke tests and demos.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use pdf_core::generate::DocOptions;
use pdf_core::raster::{ImageFormat, RenderOptions};
use pdf_core::redact::{RedactOutput, RedactionRect};
use pdf_core::watermark::{Pattern, StampPosition, WatermarkSpec};

#[derive(Parser)]
#[command(name = "pdfcore", about = "DocKosha native PDF engine CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Merge multiple PDFs into one
    Merge {
        inputs: Vec<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Split a PDF into one file per page
    Split {
        input: PathBuf,
        /// Output directory (files named page-<n>.pdf)
        #[arg(short, long)]
        out_dir: PathBuf,
    },
    /// Watermark every page
    Watermark {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Watermark lines (first = primary)
        #[arg(short, long, required = true)]
        line: Vec<String>,
        #[arg(long, default_value = "grid")]
        pattern: String,
        #[arg(long, default_value_t = -40.0, allow_negative_numbers = true)]
        rotation: f64,
        #[arg(long, default_value_t = 14.0)]
        font_size: f64,
        #[arg(long, default_value_t = 0.65)]
        opacity: f64,
    },
    /// Show page count and metadata
    Info { input: PathBuf },
    /// Convert CSV to a PDF table
    Csv {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Convert Markdown to PDF
    Md {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Render a page to PNG
    Thumb {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 480)]
        width: u32,
    },
    /// Extract all text
    Text { input: PathBuf },
    /// Redact a region (percent coords, top-left origin) on a page
    Redact {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, allow_negative_numbers = true)]
        left: f64,
        #[arg(long, allow_negative_numbers = true)]
        top: f64,
        #[arg(long, allow_negative_numbers = true)]
        width: f64,
        #[arg(long, allow_negative_numbers = true)]
        height: f64,
    },
    /// Stamp page numbers
    PageNumbers {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value = "{page} of {total}")]
        template: String,
    },
}

fn run() -> pdf_core::Result<()> {
    match Cli::parse().command {
        Command::Merge { inputs, output } => {
            let bytes: Vec<Vec<u8>> = inputs
                .iter()
                .map(fs::read)
                .collect::<std::io::Result<_>>()?;
            let refs: Vec<&[u8]> = bytes.iter().map(Vec::as_slice).collect();
            let merged = pdf_core::assembly::merge(&refs)?;
            fs::write(&output, merged)?;
            println!("merged {} files -> {}", inputs.len(), output.display());
        }
        Command::Split { input, out_dir } => {
            let parts = pdf_core::assembly::split(&fs::read(&input)?)?;
            fs::create_dir_all(&out_dir)?;
            for (i, part) in parts.iter().enumerate() {
                fs::write(out_dir.join(format!("page-{}.pdf", i + 1)), part)?;
            }
            println!("split into {} pages -> {}", parts.len(), out_dir.display());
        }
        Command::Watermark {
            input,
            output,
            line,
            pattern,
            rotation,
            font_size,
            opacity,
        } => {
            let spec = WatermarkSpec {
                pattern: if pattern == "single" {
                    Pattern::Single
                } else {
                    Pattern::Grid
                },
                rotation_deg: rotation,
                lines: line,
                font_size_pt: font_size,
                opacity,
                ..WatermarkSpec::default()
            };
            let out = pdf_core::watermark::apply_watermark(&fs::read(&input)?, &spec)?;
            fs::write(&output, out)?;
            println!("watermarked -> {}", output.display());
        }
        Command::Info { input } => {
            let bytes = fs::read(&input)?;
            let info = pdf_core::meta::validate(&bytes)?;
            let md = pdf_core::meta::get_metadata(&bytes)?;
            println!(
                "pages: {}  version: {}  encrypted: {}",
                info.page_count, info.version, info.encrypted
            );
            println!("title: {:?}  author: {:?}", md.title, md.author);
            for (i, (w, h)) in pdf_core::meta::page_sizes(&bytes)?.iter().enumerate() {
                println!("page {}: {w:.0} x {h:.0} pt", i + 1);
            }
        }
        Command::Csv { input, output } => {
            let out = pdf_core::generate::csv_to_pdf(&fs::read(&input)?, &DocOptions::default())?;
            fs::write(&output, out)?;
            println!("csv -> {}", output.display());
        }
        Command::Md { input, output } => {
            let md = fs::read_to_string(&input)?;
            let out = pdf_core::generate::markdown_to_pdf(&md, &DocOptions::default())?;
            fs::write(&output, out)?;
            println!("markdown -> {}", output.display());
        }
        Command::Thumb {
            input,
            output,
            page,
            width,
        } => {
            let opts = RenderOptions {
                target_width_px: Some(width),
                scale: 1.0,
                format: ImageFormat::Png,
            };
            let png = pdf_core::raster::render_page(&fs::read(&input)?, page, &opts)?;
            fs::write(&output, png)?;
            println!("page {page} -> {}", output.display());
        }
        Command::Text { input } => {
            use std::io::Write;
            let text = pdf_core::extract::extract_text(&fs::read(&input)?)?;
            // writeln! (unlike println!) surfaces EPIPE as an error instead of
            // panicking; main() treats BrokenPipe as success (e.g. `| head`).
            writeln!(std::io::stdout(), "{text}")?;
        }
        Command::Redact {
            input,
            output,
            page,
            left,
            top,
            width,
            height,
        } => {
            let mut map = BTreeMap::new();
            map.insert(
                page,
                vec![RedactionRect {
                    left_pct: left,
                    top_pct: top,
                    width_pct: width,
                    height_pct: height,
                }],
            );
            let RedactOutput {
                pdf,
                page_count,
                warnings,
            } = pdf_core::redact::redact(&fs::read(&input)?, &map)?;
            fs::write(&output, pdf)?;
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            println!("redacted ({page_count} pages) -> {}", output.display());
        }
        Command::PageNumbers {
            input,
            output,
            template,
        } => {
            let out = pdf_core::watermark::stamp_page_numbers(
                &fs::read(&input)?,
                &template,
                StampPosition::BottomRight,
                10.0,
                1,
            )?;
            fs::write(&output, out)?;
            println!("stamped -> {}", output.display());
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        // Downstream closed the pipe (e.g. `pdfcore text x.pdf | head`):
        // conventional success, not an error.
        Err(pdf_core::PdfCoreError::Io(err)) if err.kind() == std::io::ErrorKind::BrokenPipe => {
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("pdfcore error: {err}");
            ExitCode::FAILURE
        }
    }
}
