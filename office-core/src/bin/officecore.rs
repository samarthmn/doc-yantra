//! `officecore` — the office-core command-line interface.
//!
//! ```text
//! officecore convert <in.docx> [-o out.pdf] [--font-path DIR]...
//! officecore profile <in.docx>
//! ```
//!
//! `convert` exits 3 on an `Unsupported` envelope miss (printing the profile to
//! stderr), 2 on an unknown extension, 1 on other errors. `profile` prints the
//! DocProfile JSON to stdout.

use clap::{Parser, Subcommand};
use office_core::{ConvertOptions, OfficeError, OfficeFormat, convert, profile_with_options};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "officecore", about = "DocKosha Office->PDF engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert an Office document to PDF.
    Convert {
        /// Input `.docx` / `.pptx` / `.xlsx` file.
        input: PathBuf,
        /// Output PDF path (default: input with a `.pdf` extension).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Additional font directory to scan (repeatable).
        #[arg(long = "font-dir", alias = "font-path")]
        font_dir: Vec<PathBuf>,
        /// Authorize scanning OS-managed font directories.
        #[arg(long)]
        system_fonts: bool,
    },
    /// Print the DocProfile JSON for a document.
    Profile {
        /// Input file.
        input: PathBuf,
        /// Additional font directory to scan (repeatable).
        #[arg(long = "font-dir", alias = "font-path")]
        font_dir: Vec<PathBuf>,
        /// Authorize scanning OS-managed font directories.
        #[arg(long)]
        system_fonts: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Convert {
            input,
            output,
            font_dir,
            system_fonts,
        } => run_convert(input, output, font_dir, system_fonts),
        Command::Profile {
            input,
            font_dir,
            system_fonts,
        } => run_profile(input, font_dir, system_fonts),
    }
}

fn format_for(path: &std::path::Path) -> Option<OfficeFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    OfficeFormat::from_extension(&ext)
}

fn run_convert(
    input: PathBuf,
    output: Option<PathBuf>,
    font_dir: Vec<PathBuf>,
    system_fonts: bool,
) -> ExitCode {
    let Some(format) = format_for(&input) else {
        eprintln!("unknown or unsupported extension: {}", input.display());
        return ExitCode::from(2);
    };
    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {}: {e}", input.display());
            return ExitCode::from(1);
        }
    };
    let opts = ConvertOptions {
        font_dirs: font_dir,
        include_system_fonts: system_fonts,
        source_name: input
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string),
        ..Default::default()
    };
    match convert(&bytes, format, &opts) {
        Ok(out) => {
            let out_path = output.unwrap_or_else(|| input.with_extension("pdf"));
            if let Err(e) = std::fs::write(&out_path, &out.pdf) {
                eprintln!("cannot write {}: {e}", out_path.display());
                return ExitCode::from(1);
            }
            eprintln!("wrote {} ({} pages)", out_path.display(), out.page_count);
            ExitCode::SUCCESS
        }
        Err(err @ OfficeError::Unsupported { .. }) => {
            eprintln!("{err}");
            if let Ok(json) = profile_with_options(&bytes, format, &opts).and_then(|p| {
                serde_json::to_string_pretty(&p).map_err(|e| OfficeError::Internal(e.to_string()))
            }) {
                eprintln!("{json}");
            }
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn run_profile(input: PathBuf, font_dir: Vec<PathBuf>, system_fonts: bool) -> ExitCode {
    let Some(format) = format_for(&input) else {
        eprintln!("unknown or unsupported extension: {}", input.display());
        return ExitCode::from(2);
    };
    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {}: {e}", input.display());
            return ExitCode::from(1);
        }
    };
    let options = ConvertOptions {
        font_dirs: font_dir,
        include_system_fonts: system_fonts,
        ..ConvertOptions::default()
    };
    match profile_with_options(&bytes, format, &options) {
        Ok(p) => match serde_json::to_string_pretty(&p) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("serialization error: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
