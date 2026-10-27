//! Build script: generates the bundled-font manifest.
//!
//! Scans `office-core/fonts/**` for native-only fixture faces and adds the five
//! shared Liberation faces from the canonical offline font package. It emits a
//! `BUNDLED_FONTS` table of `(file_stem, include_bytes!(abs_path))` into
//! `$OUT_DIR/bundled_fonts.rs`.

use std::path::{Path, PathBuf};
use std::{env, fs};

const SHARED_LIBERATION_FONTS: &[&str] = &[
    "LiberationMono-Regular.ttf",
    "LiberationSans-Bold.ttf",
    "LiberationSans-BoldItalic.ttf",
    "LiberationSans-Italic.ttf",
    "LiberationSans-Regular.ttf",
];

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let fonts_dir = manifest_dir.join("fonts");
    let canonical_fonts_dir = manifest_dir.join("../packages/doc-yantra-fonts/fonts");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut fonts = Vec::new();
    collect_fonts(&fonts_dir, &mut fonts);
    fonts.extend(
        SHARED_LIBERATION_FONTS
            .iter()
            .map(|file| canonical_fonts_dir.join(file)),
    );
    // Deterministic ordering so builds are reproducible.
    fonts.sort();

    let mut code = String::from(
        "/// Bundled fonts embedded at build time: (file stem, TTF/OTF/TTC bytes).\n\
         pub static BUNDLED_FONTS: &[(&str, &[u8])] = &[\n",
    );
    for path in &fonts {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("font")
            .to_string();
        code.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            stem,
            path.display().to_string()
        ));
    }
    code.push_str("];\n");

    fs::write(out_dir.join("bundled_fonts.rs"), code).expect("write bundled_fonts.rs");
    println!("cargo:rerun-if-changed=fonts");
    println!("cargo:rerun-if-changed={}", canonical_fonts_dir.display());
}

fn collect_fonts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fonts(&path, out);
        } else if matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("ttf") | Some("otf") | Some("ttc")
        ) {
            out.push(path);
        }
    }
}
