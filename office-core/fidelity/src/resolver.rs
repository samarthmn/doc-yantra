//! Locked, content-addressed font resolution for fidelity runs.

use anyhow::{Context, Result};
use fidelity::evidence::{is_sha256, sha256_hex};
use office_core::fonts::{FontSubstitutionClass, classify_font_request};
use office_core::{ConvertOptions, FontExpectation, FontRequest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::time::Duration;

const CDN_BASE: &str = "https://cdn.jsdelivr.net/fontsource/fonts/";
const MAX_FONT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_FONT_SOURCE_ARCHIVE_BYTES: u64 = 12 * 1024 * 1024;

/// Legal, harness-only sources selected by the controlled raster
/// experiment. They are never compiled into office-core or its wasm package.
/// The full Ubuntu 0.83 face replaces Fontsource's dehinted Unicode shards for
/// regular text; Supplemental Arial replaces Word's unhinted DFont regular.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HintedSourceLocation {
    Ubuntu083Archive {
        url: &'static str,
        archive_sha256: &'static str,
        member_suffix: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HintedHarnessSource {
    family: &'static str,
    weight: u16,
    italic: bool,
    sha256: &'static str,
    location: HintedSourceLocation,
}

const HINTED_HARNESS_SOURCES: &[HintedHarnessSource] = &[
    HintedHarnessSource {
        family: "Ubuntu",
        weight: 400,
        italic: false,
        sha256: "52c1afa489ae7bfd893af6cdd9f1af258005703600449e70d338caabcff507e5",
        location: HintedSourceLocation::Ubuntu083Archive {
            url: "https://archive.ubuntu.com/ubuntu/pool/main/f/fonts-ubuntu/fonts-ubuntu_0.83.orig.tar.gz",
            archive_sha256: "1a96df0ea32bf88278ce9d24c3a31e4c3d31f4397ff7a3af6622dc488b0b830a",
            member_suffix: "ubuntu-font-family-sources-0.83/Ubuntu-R.ttf",
        },
    },
    // No Arial pin: the hinted macOS Supplemental Arial 5.01.2x is a
    // legitimate hinted source, but its raster is a *worse* match for the
    // Word truths than the unhinted Word DFonts Arial 6.80i they were
    // produced with (korean regressed up to -0.0108, measured 2026-07-20).
    // Hinting only helps when the hint program is the one Word used.
];

/// Repo-committed pan-CJK faces, hash-pinned like `fonts.lock` artifacts.
/// These are the exact OTFs hosts register in production (shipped by
/// `@samarthmn/doc-yantra` for pdf-core), so corpus runs exercise the same
/// bytes without any CDN catalog entry.
const PAN_CJK_FONTS: &[(&str, &str, &str)] = &[
    (
        "Noto Sans CJK SC",
        "NotoSansCJKsc-Regular.otf",
        "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
    ),
    (
        "Noto Sans CJK TC",
        "NotoSansCJKtc-Regular.otf",
        "dce08bd4fd91aa8aa76ed8fea4b694c2dfb8550f67871e326843212ddbeb88b4",
    ),
    (
        "Noto Sans CJK JP",
        "NotoSansCJKjp-Regular.otf",
        "68a3fc98800b2a27b371f2fb79991daf3633bd89309d4ffaa6946fd587f375b5",
    ),
    (
        "Noto Sans CJK KR",
        "NotoSansCJKkr-Regular.otf",
        "6bcb2a0703aa137e874fc2dffa85f6c21ba9a67fa329e81b8c801663af7e992a",
    ),
];

/// Serif pan-CJK faces, fetched from the pinned `Serif2.003` release tag and
/// verified against these hashes before use. They exist only for corpus
/// fidelity — production hosts register the Sans faces and the engine falls
/// back to them — so they are cached under `fonts-cache/` instead of being
/// committed (~24 MiB each).
const PAN_CJK_SERIF_FONTS: &[(&str, &str, &str)] = &[
    (
        "Noto Serif CJK SC",
        "SimplifiedChinese/NotoSerifCJKsc-Regular.otf",
        "2a2eae2628df83556c54018c41e20fa532c1b862c5256ae8b3f23feb918d12ca",
    ),
    (
        "Noto Serif CJK TC",
        "TraditionalChinese/NotoSerifCJKtc-Regular.otf",
        "234301038e76e7c35c43113785024700c4e4fe7bdce1d1fbbc42fca7e6683798",
    ),
    (
        "Noto Serif CJK JP",
        "Japanese/NotoSerifCJKjp-Regular.otf",
        "d9854c7a8ef170b5a7932558856fd64eb8de0b007cd823fed6f9f514ad2803d3",
    ),
    (
        "Noto Serif CJK KR",
        "Korean/NotoSerifCJKkr-Regular.otf",
        "77b4b741f864d27f15e90f275b17106dde90b2ad28f82bab72dc95805db5fb42",
    ),
];

/// Metric-compatible legacy clones fetched individually (not from the Noto
/// serif tree). IPAexGothic mirrors MS Gothic's design — the face the CJK
/// ground truths actually embed — where Noto Sans CJK JP only matches its
/// class.
const LEGACY_CJK_CLONES: &[(&str, &str, &str)] = &[
    (
        "IPAexGothic",
        "cache-only",
        "3b9955a5e437ffb24c41548e79296fa724822da21ee75dc1b94f6ccdb8f400dd",
    ),
    (
        "FandolSong",
        "cache-only",
        "a3820003227d4abe31fe28b493200d94a849ccfd078b50a019264f70353020b0",
    ),
];

const PAN_CJK_SERIF_BASE: &str =
    "https://raw.githubusercontent.com/notofonts/noto-cjk/Serif2.003/Serif/OTF/";
const MAX_PAN_CJK_FONT_BYTES: u64 = 32 * 1024 * 1024;

/// Word for Mac's bundled legacy East Asian fonts — the exact faces the CJK
/// ground truths were rendered with. Loaded straight from the local Office
/// installation when present (never fetched, never committed); without
/// Office the substitution chain (FandolSong, IPAexGothic, Noto CJK) takes
/// over. Each `.ttc` registers every face it contains.
const LOCAL_LEGACY_FONT_DIR: &str = "/Applications/Microsoft Word.app/Contents/Resources/DFonts";
const LOCAL_LEGACY_FONTS: &[(&[&str], &[&str])] = &[
    (&["Simsun.ttc"], &["SimSun", "NSimSun", "宋体", "新宋体"]),
    (
        &["msmincho.ttc"],
        &["MS Mincho", "MS PMincho", "ＭＳ 明朝", "ＭＳ Ｐ明朝"],
    ),
    (
        &["msgothic.ttc"],
        &[
            "MS Gothic",
            "MS PGothic",
            "MS UI Gothic",
            "ＭＳ ゴシック",
            "ＭＳ Ｐゴシック",
        ],
    ),
    (
        &["mingliu.ttc"],
        &["MingLiU", "PMingLiU", "細明體", "新細明體"],
    ),
    (
        &["batang.ttc"],
        &[
            "Batang",
            "BatangChe",
            "Gungsuh",
            "GungsuhChe",
            "바탕",
            "바탕체",
            "궁서",
        ],
    ),
    (
        &["gulim.ttc"],
        &[
            "Gulim",
            "GulimChe",
            "Dotum",
            "DotumChe",
            "굴림",
            "굴림체",
            "돋움",
            "돋움체",
        ],
    ),
    (
        &["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf"],
        &["Arial"],
    ),
    (
        &[
            "Cambria.ttc",
            "Cambriab.ttf",
            "Cambriai.ttf",
            "Cambriaz.ttf",
        ],
        &["Cambria", "Cambria Math"],
    ),
    // Supplemental Arial above is the pinned regular face. Keep Word's local
    // style-specific files only; loading its unhinted `arial.ttf` would lose
    // the raster gain proved by the controlled raster experiment and could win an equal-style tie.
    (&["arialbd.ttf", "ariali.ttf", "arialbi.ttf"], &["Arial"]),
    (
        &[
            "Calibri.ttf",
            "Calibrib.ttf",
            "Calibrii.ttf",
            "calibril.ttf",
            "calibrili.ttf",
            "Calibriz.ttf",
        ],
        &["Calibri", "Calibri Light"],
    ),
    (&["Century.ttf"], &["Century"]),
    (
        &["malgun.ttf", "malgunbd.ttf", "malgunsl.ttf"],
        &["Malgun Gothic", "Malgun Gothic Semilight", "맑은 고딕"],
    ),
    (
        &["times.ttf", "timesbd.ttf", "timesi.ttf", "timesbi.ttf"],
        &["Times New Roman"],
    ),
    (&["SimHei.ttf"], &["SimHei", "黑体"]),
    (&["Kaiti.ttf"], &["KaiTi", "楷体"]),
    (&["Fangsong.ttf"], &["FangSong", "仿宋"]),
];

const ALIASES: &[(&str, &str)] = &[
    ("arial", "arimo"),
    ("arimo", "arimo"),
    ("caladea", "caladea"),
    ("calibri", "carlito"),
    ("calibri light", "carlito"),
    ("cambria", "caladea"),
    ("cambria math", "caladea"),
    ("carlito", "carlito"),
    ("cousine", "cousine"),
    ("courier", "cousine"),
    ("courier new", "cousine"),
    ("helvetica", "arimo"),
    ("georgia", "gelasio"),
    ("liberation mono", "cousine"),
    ("liberation sans", "arimo"),
    ("liberation serif", "tinos"),
    ("noto sans math", "noto-sans-math"),
    ("noto sans symbols", "noto-sans-symbols"),
    ("noto sans symbols 2", "noto-sans-symbols-2"),
    ("segoe ui", "arimo"),
    ("times", "tinos"),
    ("times new roman", "tinos"),
    ("tinos", "tinos"),
    ("ubuntu", "ubuntu"),
    ("ubuntu mono", "ubuntu-mono"),
];

struct CatalogFont {
    id: &'static str,
    family: &'static str,
    cdn_version: &'static str,
    catalog_version: &'static str,
    weights: &'static [u16],
    subsets: &'static [&'static str],
}

const LATIN: &[&str] = &["latin", "latin-ext"];
const GELASIO_SUBSETS: &[&str] = &["latin", "latin-ext", "vietnamese"];
const LATIN_CYRILLIC_GREEK: &[&str] = &[
    "cyrillic",
    "cyrillic-ext",
    "greek",
    "greek-ext",
    "latin",
    "latin-ext",
    "vietnamese",
];
const FULL_CROSCORE: &[&str] = &[
    "cyrillic",
    "cyrillic-ext",
    "greek",
    "greek-ext",
    "hebrew",
    "latin",
    "latin-ext",
    "vietnamese",
];
const UBUNTU_SUBSETS: &[&str] = &[
    "cyrillic",
    "cyrillic-ext",
    "greek",
    "greek-ext",
    "latin",
    "latin-ext",
];

const CATALOG: &[CatalogFont] = &[
    CatalogFont {
        id: "arimo",
        family: "Arimo",
        cdn_version: "5.2.8",
        catalog_version: "v36",
        weights: &[400, 500, 600, 700],
        subsets: FULL_CROSCORE,
    },
    CatalogFont {
        id: "caladea",
        family: "Caladea",
        cdn_version: "5.2.8",
        catalog_version: "v8",
        weights: &[400, 700],
        subsets: LATIN,
    },
    CatalogFont {
        id: "carlito",
        family: "Carlito",
        cdn_version: "5.2.8",
        catalog_version: "v4",
        weights: &[400, 700],
        subsets: LATIN_CYRILLIC_GREEK,
    },
    CatalogFont {
        id: "cousine",
        family: "Cousine",
        cdn_version: "5.2.7",
        catalog_version: "v30",
        weights: &[400, 700],
        subsets: FULL_CROSCORE,
    },
    CatalogFont {
        id: "gelasio",
        family: "Gelasio",
        cdn_version: "5.2.8",
        catalog_version: "v14",
        weights: &[400, 500, 600, 700],
        subsets: GELASIO_SUBSETS,
    },
    CatalogFont {
        id: "tinos",
        family: "Tinos",
        cdn_version: "5.2.7",
        catalog_version: "v26",
        weights: &[400, 700],
        subsets: FULL_CROSCORE,
    },
    CatalogFont {
        id: "ubuntu",
        family: "Ubuntu",
        cdn_version: "5.2.8",
        catalog_version: "v21",
        weights: &[300, 400, 500, 700],
        subsets: UBUNTU_SUBSETS,
    },
    CatalogFont {
        id: "ubuntu-mono",
        family: "Ubuntu Mono",
        cdn_version: "5.2.8",
        catalog_version: "v19",
        weights: &[400, 700],
        subsets: UBUNTU_SUBSETS,
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FontLock {
    version: u32,
    #[serde(default)]
    fonts: Vec<LockedFont>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockedFont {
    id: String,
    family: String,
    cdn_version: String,
    catalog_version: String,
    subset: String,
    weight: u16,
    style: String,
    sha256: String,
    #[serde(default)]
    internal_version: Option<String>,
}

pub(crate) struct ResolvedFonts {
    pub(crate) options: ConvertOptions,
    pub(crate) families: BTreeSet<String>,
    pub(crate) demanded_families: BTreeSet<String>,
}

pub(crate) fn lock_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts.lock")
}

pub(crate) fn cache_dir() -> std::path::PathBuf {
    if let Some(directory) = std::env::var_os("DOC_YANTRA_FIDELITY_FONT_CACHE") {
        return std::path::PathBuf::from(directory);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts-cache")
}

pub(crate) fn resolve_for_case(
    case_name: &str,
    source: &[u8],
    format: office_core::OfficeFormat,
    base_options: &ConvertOptions,
) -> Result<ResolvedFonts> {
    let lock_path = lock_path();
    let lock_contents = fs::read_to_string(&lock_path)
        .with_context(|| format!("read fidelity font lock {}", lock_path.display()))?;
    let lock: FontLock = toml::from_str(&lock_contents)
        .with_context(|| format!("parse fidelity font lock {}", lock_path.display()))?;
    let requests = office_core::font_requests(source, format, base_options)
        .with_context(|| format!("discover fonts for fidelity case {case_name}"))?;
    let (mut provider_requests, satisfied_families) =
        provider_requests(source, format, base_options, &requests)?;
    let fetcher = HttpFetcher::new()?;
    let mut pinned_options = base_options.clone();
    let pinned_families = attach_pinned_hinted_sources(&requests, &mut pinned_options, &fetcher)?;
    provider_requests.retain(|request| pinned_harness_source_for(request).is_none());
    let mut resolved = resolve_requests_with_fetch(
        case_name,
        &provider_requests,
        &lock,
        &cache_dir(),
        &pinned_options,
        |url| fetcher.fetch(url),
    )?;
    resolved.families.extend(satisfied_families);
    resolved.families.extend(pinned_families);
    attach_local_legacy_faces(&requests, &mut resolved)?;
    attach_pan_cjk_faces(case_name, &requests, &mut resolved, &fetcher)?;
    resolved.demanded_families = resolved.families.clone();
    Ok(resolved)
}

fn pinned_harness_source_for(request: &FontRequest) -> Option<&'static HintedHarnessSource> {
    HINTED_HARNESS_SOURCES.iter().find(|source| {
        request.weight == source.weight
            && request.italic == source.italic
            && request.stretch == 5
            && std::iter::once(request.family.as_str())
                .chain(request.alt_families.iter().map(String::as_str))
                .any(|family| normalize_family(family) == normalize_family(source.family))
    })
}

fn attach_pinned_hinted_sources(
    requests: &[FontRequest],
    options: &mut ConvertOptions,
    fetcher: &HttpFetcher,
) -> Result<BTreeSet<String>> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for source in requests.iter().filter_map(pinned_harness_source_for) {
        if seen.insert(source.sha256) {
            selected.push(source);
        }
    }

    let mut families = BTreeSet::new();
    // Insert in reverse so HINTED_HARNESS_SOURCES/request order remains the
    // FontDb tie-break order ahead of any provider artifacts.
    for source in selected.into_iter().rev() {
        let bytes = load_pinned_harness_font(source, fetcher)?;
        let expectation = FontExpectation {
            family: source.family.to_string(),
            weight: source.weight,
            italic: source.italic,
            stretch: 5,
            sha256: Some(source.sha256.to_string()),
            ..FontExpectation::default()
        };
        office_core::fonts::validate_font(&bytes, Some(&expectation)).with_context(|| {
            format!(
                "validate pinned hinted fidelity source {} ({})",
                source.family, source.sha256
            )
        })?;
        prepend_pinned_harness_font(options, bytes, source.sha256);
        families.insert(source.family.to_string());
    }
    Ok(families)
}

fn prepend_pinned_harness_font(options: &mut ConvertOptions, bytes: Vec<u8>, sha256: &str) {
    options.font_bytes.insert(0, bytes);
    options.font_hash_pins.insert(0, sha256.to_string());
}

fn load_pinned_harness_font(
    source: &HintedHarnessSource,
    fetcher: &HttpFetcher,
) -> Result<Vec<u8>> {
    let cache_path = cache_dir().join(format!("{}.ttf", source.sha256));
    match fs::read(&cache_path) {
        Ok(bytes) => {
            if sha256_hex(&bytes) == source.sha256 {
                return Ok(bytes);
            }
            fs::remove_file(&cache_path).with_context(|| {
                format!(
                    "remove corrupt pinned font cache entry {}",
                    cache_path.display()
                )
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read pinned font cache entry {}", cache_path.display()));
        }
    }

    let bytes = match source.location {
        HintedSourceLocation::Ubuntu083Archive {
            url,
            archive_sha256,
            member_suffix,
        } => {
            let archive = fetcher
                .fetch_with_limit(url, MAX_FONT_SOURCE_ARCHIVE_BYTES)
                .with_context(|| format!("fetch pinned Ubuntu 0.83 source archive {url}"))?;
            let actual = sha256_hex(&archive);
            if actual != archive_sha256 {
                anyhow::bail!(
                    "pinned Ubuntu source archive hash mismatch: expected {archive_sha256}, found {actual}"
                );
            }
            extract_tar_gz_member(&archive, member_suffix)?
        }
    };
    let actual = sha256_hex(&bytes);
    if actual != source.sha256 {
        anyhow::bail!(
            "pinned hinted {} hash mismatch: expected {}, found {actual}",
            source.family,
            source.sha256
        );
    }
    fs::create_dir_all(cache_dir()).context("create fidelity font cache")?;
    fs::write(&cache_path, &bytes)
        .with_context(|| format!("cache pinned hinted font {}", cache_path.display()))?;
    Ok(bytes)
}

fn extract_tar_gz_member(archive: &[u8], member_suffix: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(archive));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("read Ubuntu source archive")? {
        let mut entry = entry.context("read Ubuntu source archive entry")?;
        let path = entry.path().context("read Ubuntu source member path")?;
        if !path.ends_with(member_suffix) {
            continue;
        }
        if entry.size() > MAX_FONT_BYTES {
            anyhow::bail!("pinned Ubuntu font exceeds {MAX_FONT_BYTES} bytes");
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .by_ref()
            .take(MAX_FONT_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("extract pinned Ubuntu font")?;
        if bytes.len() as u64 > MAX_FONT_BYTES {
            anyhow::bail!("pinned Ubuntu font exceeds {MAX_FONT_BYTES} bytes");
        }
        return Ok(bytes);
    }
    anyhow::bail!("pinned Ubuntu source archive lacks {member_suffix}")
}

/// Registers the machine's real legacy East Asian fonts for every request
/// they satisfy. The pan-CJK substitutes still attach afterwards: exact
/// family resolution prefers the real face, while the Noto faces stay
/// registered as the wide-coverage fallback net.
fn attach_local_legacy_faces(requests: &[FontRequest], resolved: &mut ResolvedFonts) -> Result<()> {
    let dir = Path::new(LOCAL_LEGACY_FONT_DIR);
    if !dir.is_dir() {
        return Ok(());
    }
    let mut loaded_files: BTreeSet<&str> = BTreeSet::new();
    for request in requests {
        for name in std::iter::once(request.family.as_str())
            .chain(request.alt_families.iter().map(String::as_str))
        {
            let key = office_core::fonts::family_key(name);
            let Some((files, families)) = LOCAL_LEGACY_FONTS.iter().find(|(_, families)| {
                families
                    .iter()
                    .any(|family| office_core::fonts::family_key(family) == key)
            }) else {
                continue;
            };
            let mut any = false;
            for file in *files {
                let path = dir.join(file);
                if !path.is_file() || !loaded_files.insert(file) {
                    any = any || path.is_file();
                    continue;
                }
                let bytes = fs::read(&path)
                    .with_context(|| format!("read local legacy font {}", path.display()))?;
                office_core::fonts::validate_font(&bytes, None)
                    .with_context(|| format!("validate local legacy font {}", path.display()))?;
                let pin = sha256_hex(&bytes);
                resolved.options.font_bytes.push(bytes);
                resolved.options.font_hash_pins.push(pin);
                any = true;
            }
            if any {
                for family in families.iter().filter(|family| family.is_ascii()) {
                    resolved.families.insert((*family).to_string());
                }
            }
            break;
        }
    }
    Ok(())
}

/// Provisions the repo-committed pan-CJK OTFs for every request whose
/// substitution target is a Noto Sans CJK face, mirroring what production
/// hosts register through the font provider.
fn attach_pan_cjk_faces(
    case_name: &str,
    requests: &[FontRequest],
    resolved: &mut ResolvedFonts,
    fetcher: &HttpFetcher,
) -> Result<()> {
    let mut targets = BTreeSet::new();
    for request in requests {
        if let Some(target) = pan_cjk_request_target(request) {
            targets.insert(target);
        }
    }
    for target in targets {
        let (family, bytes, pinned) = if let Some((family, file, pinned)) = PAN_CJK_FONTS
            .iter()
            .find(|(family, _, _)| *family == target)
        {
            if pin_already_attached(resolved, pinned) {
                resolved.families.insert((*family).to_string());
                continue;
            }
            let path = pan_cjk_dir().join(file);
            let bytes = fs::read(&path)
                .with_context(|| format!("read committed pan-CJK font {}", path.display()))?;
            let actual = sha256_hex(&bytes);
            if actual != *pinned {
                anyhow::bail!(
                    "committed pan-CJK font {} hash mismatch: expected {pinned}, found {actual}; update PAN_CJK_FONTS together with the font",
                    path.display()
                );
            }
            (family, bytes, pinned)
        } else if let Some((family, file, pinned)) = PAN_CJK_SERIF_FONTS
            .iter()
            .find(|(family, _, _)| *family == target)
        {
            if pin_already_attached(resolved, pinned) {
                resolved.families.insert((*family).to_string());
                continue;
            }
            let bytes = fetch_pinned_serif_face(fetcher, file, pinned)?;
            (family, bytes, pinned)
        } else if let Some((family, _, pinned)) = LEGACY_CJK_CLONES
            .iter()
            .find(|(family, _, _)| *family == target)
        {
            if pin_already_attached(resolved, pinned) {
                resolved.families.insert((*family).to_string());
                continue;
            }
            let cache_path = cache_dir().join(format!("{pinned}.ttf"));
            let bytes = fs::read(&cache_path).with_context(|| {
                format!(
                    "legacy CJK clone {family} must be seeded at {} (hash {pinned})",
                    cache_path.display()
                )
            })?;
            let actual = sha256_hex(&bytes);
            if actual != *pinned {
                anyhow::bail!(
                    "legacy CJK clone {family} cache hash mismatch: expected {pinned}, found {actual}"
                );
            }
            (family, bytes, pinned)
        } else {
            anyhow::bail!(
                "case {case_name} needs {target} but no pan-CJK OTF is provisioned for it"
            );
        };
        let expectation = FontExpectation {
            family: (*family).to_string(),
            weight: 400,
            italic: false,
            stretch: 5,
            sha256: Some((*pinned).to_string()),
            ..FontExpectation::default()
        };
        office_core::fonts::validate_font(&bytes, Some(&expectation))
            .with_context(|| format!("validate pan-CJK font {family}"))?;
        resolved.options.font_bytes.push(bytes);
        resolved.options.font_hash_pins.push((*pinned).to_string());
        resolved.families.insert((*family).to_string());
    }
    Ok(())
}

fn pin_already_attached(resolved: &ResolvedFonts, pinned: &str) -> bool {
    resolved
        .options
        .font_hash_pins
        .iter()
        .any(|hash| hash.eq_ignore_ascii_case(pinned))
}

/// Returns a serif pan-CJK OTF from `fonts-cache/`, fetching it from the
/// pinned notofonts release on first use. The cache key is the pinned hash,
/// so a stale or tampered cache entry re-fetches instead of failing the run.
fn fetch_pinned_serif_face(fetcher: &HttpFetcher, file: &str, pinned: &str) -> Result<Vec<u8>> {
    let cache_dir = cache_dir();
    let cache_path = cache_dir.join(format!("{pinned}.ttf"));
    if let Ok(bytes) = fs::read(&cache_path)
        && sha256_hex(&bytes) == pinned
    {
        return Ok(bytes);
    }
    let url = format!("{PAN_CJK_SERIF_BASE}{file}");
    let bytes = fetcher
        .fetch_with_limit(&url, MAX_PAN_CJK_FONT_BYTES)
        .with_context(|| format!("fetch serif pan-CJK font {url}"))?;
    let actual = sha256_hex(&bytes);
    if actual != pinned {
        anyhow::bail!(
            "serif pan-CJK font {url} hash mismatch: expected {pinned}, found {actual}; update PAN_CJK_SERIF_FONTS together with the pinned tag"
        );
    }
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create fidelity font cache {}", cache_dir.display()))?;
    fs::write(&cache_path, &bytes).with_context(|| format!("seed fidelity cache for {url}"))?;
    Ok(bytes)
}

fn pan_cjk_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/doc-yantra-fonts/fonts")
}

/// The pan-CJK target for a request, honoring font-table alternates.
fn pan_cjk_request_target(request: &FontRequest) -> Option<&'static str> {
    std::iter::once(request.family.as_str())
        .chain(request.alt_families.iter().map(String::as_str))
        .find_map(office_core::fonts::pan_cjk_substitution_target)
}

pub(crate) fn refresh_lock(sources: &[(String, Vec<u8>)]) -> Result<usize> {
    let fetcher = HttpFetcher::new()?;
    let mut desired = BTreeMap::new();
    for (case_name, source) in sources {
        let requests = office_core::font_requests(
            source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .with_context(|| format!("discover fonts for fidelity case {case_name}"))?;
        let (requests, _) = provider_requests(
            source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
            &requests,
        )?;
        for request in &requests {
            for font in desired_artifacts(request)? {
                desired.entry(font.url()).or_insert(font);
            }
        }
    }
    for request in permanent_lock_requests() {
        for font in desired_artifacts(&request)? {
            desired.entry(font.url()).or_insert(font);
        }
    }

    let cache_dir = cache_dir();
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create fidelity font cache {}", cache_dir.display()))?;
    let mut fonts = Vec::with_capacity(desired.len());
    for (_, mut font) in desired {
        let url = font.url();
        let bytes = fetcher
            .fetch(&url)
            .with_context(|| format!("fetch font-lock artifact {url}"))?;
        let identity = validate_unlocked_font(&bytes, &font)?;
        font.sha256 = identity.sha256;
        font.internal_version = identity.version;
        fs::write(cache_dir.join(format!("{}.ttf", font.sha256)), &bytes)
            .with_context(|| format!("seed fidelity cache for {url}"))?;
        fonts.push(font);
    }
    fonts.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.weight.cmp(&right.weight))
            .then(left.style.cmp(&right.style))
            .then(left.subset.cmp(&right.subset))
    });
    let lock = FontLock { version: 1, fonts };
    let serialized = toml::to_string_pretty(&lock).context("serialize fidelity font lock")?;
    fs::write(lock_path(), serialized).context("write fidelity fonts.lock")?;
    Ok(lock.fonts.len())
}

fn permanent_lock_requests() -> Vec<FontRequest> {
    ["Arial", "Times New Roman", "Courier New", "Georgia"]
        .into_iter()
        .map(|family| FontRequest {
            family: family.into(),
            alt_families: Vec::new(),
            weight: 400,
            italic: false,
            stretch: 5,
            slots: vec![office_core::FontSlot::Ascii, office_core::FontSlot::HAnsi],
            panose: None,
            family_class: None,
            pitch: None,
            charset: None,
            unicode_ranges: Vec::new(),
            scripts: vec!["latin".into()],
            vf_coords: Vec::new(),
        })
        .collect()
}

struct HttpFetcher {
    client: reqwest::blocking::Client,
}

impl HttpFetcher {
    fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .context("build fidelity font HTTP client")?;
        Ok(HttpFetcher { client })
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        self.fetch_with_limit(url, MAX_FONT_BYTES)
    }

    fn fetch_with_limit(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            anyhow::bail!("fidelity font URL must use HTTPS: {url}");
        }
        let mut response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            anyhow::bail!("font response exceeds {limit} bytes: {url}");
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read {url}"))?;
        if bytes.len() as u64 > limit {
            anyhow::bail!("font response exceeds {limit} bytes: {url}");
        }
        Ok(bytes)
    }
}

fn resolve_requests_with_fetch<F>(
    case_name: &str,
    requests: &[FontRequest],
    lock: &FontLock,
    cache_dir: &Path,
    base_options: &ConvertOptions,
    fetch: F,
) -> Result<ResolvedFonts>
where
    F: Fn(&str) -> Result<Vec<u8>>,
{
    if lock.version != 1 {
        anyhow::bail!("unsupported fidelity font lock version {}", lock.version);
    }
    let mut options = base_options.clone();
    let mut families = BTreeSet::new();
    let mut supplied_hashes = options
        .font_hash_pins
        .iter()
        .map(|hash| hash.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();

    for request in requests {
        let Some(id) = mapped_id(request) else {
            continue;
        };
        let selected = select_locked_fonts(request, id, lock)?;
        for locked in selected {
            if !is_sha256(&locked.sha256) {
                anyhow::bail!(
                    "fonts.lock has invalid SHA-256 for {} {}/{}/{}/{}",
                    locked.family,
                    locked.subset,
                    locked.weight,
                    locked.style,
                    locked.cdn_version
                );
            }
            if supplied_hashes.contains(&locked.sha256) {
                families.insert(locked.family.clone());
                continue;
            }
            let cache_path = cache_dir.join(format!("{}.ttf", locked.sha256));
            let cached = match fs::read(&cache_path) {
                Ok(bytes) if sha256_hex(&bytes) == locked.sha256 => Some(bytes),
                Ok(_) => {
                    fs::remove_file(&cache_path).with_context(|| {
                        format!("remove corrupt font cache entry {}", cache_path.display())
                    })?;
                    None
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("read font cache entry {}", cache_path.display())
                    });
                }
            };
            let bytes = match cached {
                Some(bytes) => bytes,
                None => {
                    let url = locked.url();
                    let bytes = fetch(&url).map_err(|error| {
                        anyhow::anyhow!(
                            "case {case_name} requires {}; network unavailable and fonts-cache is cold: {error:#}",
                            request.family
                        )
                    })?;
                    let actual = sha256_hex(&bytes);
                    if actual != locked.sha256 {
                        anyhow::bail!(
                            "font lock hash mismatch for {url}: expected {}, received {actual}",
                            locked.sha256
                        );
                    }
                    validate_locked_font(&bytes, locked)?;
                    fs::create_dir_all(cache_dir).with_context(|| {
                        format!("create fidelity font cache {}", cache_dir.display())
                    })?;
                    fs::write(&cache_path, &bytes).with_context(|| {
                        format!("write fidelity font cache entry {}", cache_path.display())
                    })?;
                    bytes
                }
            };
            validate_locked_font(&bytes, locked)?;
            options.font_bytes.push(bytes);
            options.font_hash_pins.push(locked.sha256.clone());
            supplied_hashes.insert(locked.sha256.clone());
            families.insert(locked.family.clone());
        }
    }

    Ok(ResolvedFonts {
        options,
        families,
        demanded_families: mapped_families(requests),
    })
}

impl LockedFont {
    fn url(&self) -> String {
        format!(
            "{CDN_BASE}{}@{}/{}-{}-{}.ttf",
            self.id, self.cdn_version, self.subset, self.weight, self.style
        )
    }
}

fn normalize_family(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(test)]
fn alias_map() -> BTreeMap<String, String> {
    ALIASES
        .iter()
        .map(|(family, id)| ((*family).to_string(), (*id).to_string()))
        .collect()
}

fn mapped_id(request: &FontRequest) -> Option<&'static str> {
    // Pan-CJK families never map to CDN artifacts: they resolve to the
    // repo-committed OTFs in `attach_pan_cjk_faces`, and the class fallback
    // must not shadow them with a Latin-only face.
    if pan_cjk_request_target(request).is_some() {
        return None;
    }
    std::iter::once(request.family.as_str())
        .chain(request.alt_families.iter().map(String::as_str))
        .find_map(|family| {
            let family = normalize_family(family);
            ALIASES
                .iter()
                .find_map(|(known, id)| (*known == family).then_some(*id))
        })
        .or_else(|| {
            classify_font_request(request).map(|classing| match classing.class {
                FontSubstitutionClass::Serif => "tinos",
                FontSubstitutionClass::Sans => "arimo",
                FontSubstitutionClass::Mono => "cousine",
            })
        })
}

fn mapped_families(requests: &[FontRequest]) -> BTreeSet<String> {
    requests
        .iter()
        .filter_map(mapped_id)
        .filter_map(|id| catalog_font(id).ok())
        .map(|font| font.family.to_string())
        .collect()
}

fn provider_requests(
    source: &[u8],
    format: office_core::OfficeFormat,
    base_options: &ConvertOptions,
    requests: &[FontRequest],
) -> Result<(Vec<FontRequest>, BTreeSet<String>)> {
    // Only the resolved font database is needed; open the format's own
    // document type so embedded faces surface for every corpus format.
    let fonts = match format {
        office_core::OfficeFormat::Pptx => {
            office_core::pptx::PptxDoc::open(source, base_options)
                .context("open presentation for fidelity provider resolution")?
                .fonts
        }
        office_core::OfficeFormat::Xlsx => {
            office_core::xlsx::XlsxDoc::open(source, base_options)
                .context("open workbook for fidelity provider resolution")?
                .fonts
        }
        _ => {
            office_core::layout::WordDoc::open(source, base_options)
                .context("open document for fidelity provider resolution")?
                .fonts
        }
    };
    let document_fonts = &fonts;
    let mut provider = Vec::new();
    let mut satisfied = BTreeSet::new();
    for request in requests {
        let Some(id) = mapped_id(request) else {
            continue;
        };
        let native = office_core::fonts::resolve_exact_or_substitute_with_stretch(
            document_fonts,
            &request.family,
            request.weight,
            request.italic,
            request.stretch,
        );
        if let Some(face) = native.and_then(|face| document_fonts.face(face)) {
            satisfied.insert(face.family_name.clone());
        } else {
            catalog_font(id)?;
            provider.push(request.clone());
        }
    }
    Ok((provider, satisfied))
}

fn catalog_font(id: &str) -> Result<&'static CatalogFont> {
    CATALOG
        .iter()
        .find(|font| font.id == id)
        .with_context(|| format!("fidelity resolver has no catalog entry for {id}"))
}

fn requested_subsets(request: &FontRequest, available: &[&str]) -> Vec<String> {
    let mut selected = BTreeSet::from(["latin"]);
    for script in &request.scripts {
        let script = script.to_ascii_lowercase().replace('_', "-");
        match script.as_str() {
            "latin" => {
                selected.insert("latin-ext");
                selected.insert("vietnamese");
            }
            "cyrillic" => {
                selected.insert("cyrillic");
                selected.insert("cyrillic-ext");
            }
            "greek" => {
                selected.insert("greek");
                selected.insert("greek-ext");
            }
            "hebrew" => {
                selected.insert("hebrew");
            }
            _ => {}
        }
    }
    selected
        .into_iter()
        .filter(|subset| available.contains(subset))
        .map(str::to_string)
        .collect()
}

fn desired_artifacts(request: &FontRequest) -> Result<Vec<LockedFont>> {
    let Some(id) = mapped_id(request) else {
        return Ok(Vec::new());
    };
    let catalog = catalog_font(id)?;
    let weight = catalog
        .weights
        .iter()
        .copied()
        .min_by_key(|weight| (i32::from(*weight) - i32::from(request.weight)).abs())
        .expect("catalog weights are non-empty");
    let style = if request.italic { "italic" } else { "normal" };
    Ok(requested_subsets(request, catalog.subsets)
        .into_iter()
        .map(|subset| LockedFont {
            id: catalog.id.to_string(),
            family: catalog.family.to_string(),
            cdn_version: catalog.cdn_version.to_string(),
            catalog_version: catalog.catalog_version.to_string(),
            subset,
            weight,
            style: style.to_string(),
            sha256: String::new(),
            internal_version: None,
        })
        .collect())
}

fn select_locked_fonts<'a>(
    request: &FontRequest,
    id: &str,
    lock: &'a FontLock,
) -> Result<Vec<&'a LockedFont>> {
    let candidates = lock
        .fonts
        .iter()
        .filter(|font| font.id == id)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        anyhow::bail!(
            "fonts.lock has no artifacts for {} (mapped from {}); run `cargo run -p fidelity -- lock-fonts`",
            id,
            request.family
        );
    }
    let weight = candidates
        .iter()
        .map(|font| font.weight)
        .min_by_key(|weight| (i32::from(*weight) - i32::from(request.weight)).abs())
        .expect("non-empty candidates");
    let preferred_style = if request.italic { "italic" } else { "normal" };
    let style = if candidates
        .iter()
        .any(|font| font.weight == weight && font.style == preferred_style)
    {
        preferred_style
    } else {
        candidates
            .iter()
            .find(|font| font.weight == weight)
            .map(|font| font.style.as_str())
            .expect("selected weight exists")
    };
    let face_candidates = candidates
        .into_iter()
        .filter(|font| font.weight == weight && font.style == style)
        .collect::<Vec<_>>();
    let available = face_candidates
        .iter()
        .map(|font| font.subset.as_str())
        .collect::<Vec<_>>();
    let subsets = requested_subsets(request, &available)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let selected = face_candidates
        .iter()
        .copied()
        .filter(|font| subsets.contains(font.subset.as_str()))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        anyhow::bail!(
            "fonts.lock has no matching subset for {} weight={} style={style}",
            request.family,
            request.weight
        );
    }
    Ok(selected)
}

fn validate_locked_font(bytes: &[u8], locked: &LockedFont) -> Result<()> {
    let expectation = FontExpectation {
        family: locked.family.clone(),
        weight: locked.weight,
        italic: locked.style == "italic",
        stretch: 5,
        version: locked.internal_version.clone(),
        sha256: Some(locked.sha256.clone()),
        ..FontExpectation::default()
    };
    office_core::fonts::validate_font(bytes, Some(&expectation))
        .with_context(|| format!("validate locked font {}", locked.url()))?;
    Ok(())
}

fn validate_unlocked_font(bytes: &[u8], font: &LockedFont) -> Result<office_core::FontIdentity> {
    let expectation = FontExpectation {
        family: font.family.clone(),
        weight: font.weight,
        italic: font.style == "italic",
        stretch: 5,
        ..FontExpectation::default()
    };
    office_core::fonts::validate_font(bytes, Some(&expectation))
        .with_context(|| format!("validate fetched font {}", font.url()))
}

#[cfg(test)]
fn assert_js_alias_parity(source: &str) -> Result<()> {
    let block = [
        "const DEFAULT_ALIASES = Object.freeze({",
        "export const DEFAULT_FONTSOURCE_ALIASES = Object.freeze({",
    ]
    .into_iter()
    .find_map(|marker| source.split_once(marker))
    .and_then(|(_, rest)| rest.split_once("});"))
    .map(|(block, _)| block)
    .context("locate the provider alias table")?;
    let mut parsed = BTreeMap::new();
    for line in block.lines() {
        let line = line.trim().trim_end_matches(',');
        if line.is_empty() {
            continue;
        }
        let (family, id) = line
            .split_once(':')
            .with_context(|| format!("parse provider alias line {line:?}"))?;
        parsed.insert(unquote(family.trim()), unquote(id.trim()));
    }
    let expected = alias_map();
    if parsed != expected {
        anyhow::bail!("Rust fidelity aliases differ from the shipped Node provider");
    }
    Ok(())
}

#[cfg(test)]
fn unquote(value: &str) -> String {
    value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or(value)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidelity::evidence::sha256_hex;
    use office_core::{FontSlot, ResolutionMode};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);
    const CARLITO: &[u8] = include_bytes!("../../fonts/carlito/Carlito-Regular.ttf");

    fn request() -> FontRequest {
        FontRequest {
            family: "Calibri".into(),
            alt_families: Vec::new(),
            weight: 400,
            italic: false,
            stretch: 5,
            slots: vec![FontSlot::Ascii, FontSlot::HAnsi],
            panose: None,
            family_class: None,
            pitch: None,
            charset: None,
            unicode_ranges: Vec::new(),
            scripts: vec!["latin".into()],
            vf_coords: Vec::new(),
        }
    }

    fn locked_font(sha256: String) -> LockedFont {
        LockedFont {
            id: "carlito".into(),
            family: "Carlito".into(),
            cdn_version: "5.2.8".into(),
            catalog_version: "v4".into(),
            subset: "latin".into(),
            weight: 400,
            style: "normal".into(),
            sha256,
            internal_version: None,
        }
    }

    fn lock(font: LockedFont) -> FontLock {
        FontLock {
            version: 1,
            fonts: vec![font],
        }
    }

    fn test_cache(label: &str) -> std::path::PathBuf {
        let serial = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../tmp/code-review-1-resolver-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create repo-local resolver test cache");
        path
    }

    fn base_options() -> ConvertOptions {
        ConvertOptions {
            resolution_mode: ResolutionMode::Compatible,
            ..ConvertOptions::default()
        }
    }

    #[test]
    fn pan_cjk_families_bypass_the_cdn_catalog() {
        let mut simsun = request();
        simsun.family = "SimSun".into();
        simsun.family_class = Some("roman".into());
        assert_eq!(mapped_id(&simsun), None);

        let mut alt = request();
        alt.family = "Unknown Grid".into();
        alt.alt_families = vec!["MS Mincho".into()];
        alt.panose = None;
        assert_eq!(pan_cjk_request_target(&alt), Some("Noto Serif CJK JP"));
    }

    #[test]
    fn pan_cjk_faces_resolve_offline_from_the_committed_otfs() {
        let mut malgun = request();
        malgun.family = "Malgun Gothic".into();
        let mut resolved = ResolvedFonts {
            options: base_options(),
            families: BTreeSet::new(),
            demanded_families: BTreeSet::new(),
        };
        let fetcher = HttpFetcher::new().expect("build fetcher");
        attach_pan_cjk_faces("fixture", &[malgun], &mut resolved, &fetcher)
            .expect("provision committed pan-CJK face");
        assert!(resolved.families.contains("Noto Sans CJK KR"));
        assert_eq!(resolved.options.font_bytes.len(), 1);
        assert_eq!(
            resolved.options.font_hash_pins,
            vec!["6bcb2a0703aa137e874fc2dffa85f6c21ba9a67fa329e81b8c801663af7e992a".to_string()]
        );
        assert_eq!(
            sha256_hex(&resolved.options.font_bytes[0]),
            resolved.options.font_hash_pins[0]
        );
    }

    #[test]
    fn requested_family_precedes_font_table_alternates() {
        let mut request = request();
        request.family = "Ubuntu".into();
        request.alt_families = vec!["Calibri".into()];

        assert_eq!(mapped_id(&request), Some("ubuntu"));
    }

    #[test]
    fn georgia_uses_its_metric_compatible_gelasio_alias() {
        let mut request = request();
        request.family = "Georgia".into();
        assert_eq!(mapped_id(&request), Some("gelasio"));
    }

    #[test]
    fn unmapped_families_use_font_table_class_metadata() {
        let mut serif = request();
        serif.family = "Unknown Serif".into();
        serif.family_class = Some("roman".into());
        assert_eq!(mapped_id(&serif), Some("tinos"));

        let mut sans = request();
        sans.family = "Unknown Sans".into();
        sans.panose = Some("020B0604020202020204".into());
        assert_eq!(mapped_id(&sans), Some("arimo"));

        let mut mono = request();
        mono.family = "Unknown Mono".into();
        mono.pitch = Some("fixed".into());
        assert_eq!(mapped_id(&mono), Some("cousine"));
    }

    #[test]
    fn permanent_lock_plan_covers_app_asset_fallbacks_and_georgia() {
        let ids = permanent_lock_requests()
            .iter()
            .filter_map(mapped_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids,
            BTreeSet::from(["arimo", "cousine", "gelasio", "tinos"])
        );
    }

    #[test]
    fn fetched_bytes_must_match_the_committed_lock() {
        let cache = test_cache("hash-mismatch");
        let error = resolve_requests_with_fetch(
            "fixture",
            &[request()],
            &lock(locked_font("0".repeat(64))),
            &cache,
            &base_options(),
            |_| Ok(CARLITO.to_vec()),
        )
        .err()
        .expect("mismatched bytes must fail");
        fs::remove_dir_all(&cache).expect("remove repo-local resolver test cache");
        let message = format!("{error:#}");
        assert!(message.contains("font lock hash mismatch"), "{message}");
    }

    #[test]
    fn cold_offline_cache_names_the_case_and_required_family() {
        let cache = test_cache("cold-offline");
        let error = resolve_requests_with_fetch(
            "gdocs-anchors-toc",
            &[request()],
            &lock(locked_font(sha256_hex(CARLITO))),
            &cache,
            &base_options(),
            |_| anyhow::bail!("network unavailable"),
        )
        .err()
        .expect("cold offline resolution must fail");
        fs::remove_dir_all(&cache).expect("remove repo-local resolver test cache");
        let message = format!("{error:#}");
        assert!(
            message.contains("case gdocs-anchors-toc requires Calibri"),
            "{message}"
        );
        assert!(message.contains("fonts-cache is cold"), "{message}");
    }

    #[test]
    fn warm_cache_resolves_without_network() {
        let cache = test_cache("warm-offline");
        let sha256 = sha256_hex(CARLITO);
        fs::write(cache.join(format!("{sha256}.ttf")), CARLITO).expect("seed warm cache");
        let resolved = resolve_requests_with_fetch(
            "fixture",
            &[request()],
            &lock(locked_font(sha256.clone())),
            &cache,
            &base_options(),
            |_| panic!("warm cache must not fetch"),
        )
        .expect("warm resolution");
        fs::remove_dir_all(&cache).expect("remove repo-local resolver test cache");
        assert_eq!(resolved.options.font_bytes, vec![CARLITO.to_vec()]);
        assert_eq!(resolved.options.font_hash_pins, vec![sha256]);
        assert!(resolved.families.contains("Carlito"));
    }

    #[test]
    fn locked_cold_fetch_and_offline_warm_resolution_are_identical() {
        use std::cell::Cell;

        let cache = test_cache("cold-warm-parity");
        let sha256 = sha256_hex(CARLITO);
        let lock = lock(locked_font(sha256));
        let fetches = Cell::new(0usize);
        let cold = resolve_requests_with_fetch(
            "fixture",
            &[request()],
            &lock,
            &cache,
            &base_options(),
            |_| {
                fetches.set(fetches.get() + 1);
                Ok(CARLITO.to_vec())
            },
        )
        .expect("hash-verified cold resolution");
        let warm = resolve_requests_with_fetch(
            "fixture",
            &[request()],
            &lock,
            &cache,
            &base_options(),
            |_| panic!("offline warm resolution must not fetch"),
        )
        .expect("offline warm resolution");
        fs::remove_dir_all(&cache).expect("remove repo-local resolver test cache");

        assert_eq!(fetches.get(), 1);
        assert_eq!(cold.options.font_bytes, warm.options.font_bytes);
        assert_eq!(cold.options.font_hash_pins, warm.options.font_hash_pins);
        assert_eq!(cold.families, warm.families);
    }

    #[test]
    fn rust_aliases_match_the_shipped_node_provider() {
        let source = include_str!("../../wasm/fontsource-provider.cjs");
        assert_js_alias_parity(source).expect("alias maps must stay in lockstep");
    }

    #[test]
    fn rust_aliases_match_the_shipped_web_provider() {
        let source = include_str!("../../wasm/fontsource-provider-web.js");
        assert_js_alias_parity(source).expect("web alias map must stay in lockstep");
    }

    #[test]
    fn catalog_uses_family_specific_immutable_cdn_versions() {
        let versions = CATALOG
            .iter()
            .map(|font| (font.id, font.cdn_version))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(versions["arimo"], "5.2.8");
        assert_eq!(versions["caladea"], "5.2.8");
        assert_eq!(versions["carlito"], "5.2.8");
        assert_eq!(versions["cousine"], "5.2.7");
        assert_eq!(versions["gelasio"], "5.2.8");
        assert_eq!(versions["tinos"], "5.2.7");
        assert_eq!(versions["ubuntu"], "5.2.8");
        assert_eq!(versions["ubuntu-mono"], "5.2.8");
        let catalog_versions = CATALOG
            .iter()
            .map(|font| (font.id, font.catalog_version))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(catalog_versions["ubuntu"], "v21");
        assert_eq!(catalog_versions["ubuntu-mono"], "v19");
        assert_eq!(catalog_versions["gelasio"], "v14");
    }

    #[test]
    fn task5_hinted_sources_are_narrowly_pinned_to_ubuntu_regular_and_arial() {
        let mut ubuntu = request();
        ubuntu.family = "Ubuntu".into();
        let ubuntu_source = pinned_harness_source_for(&ubuntu).expect("Ubuntu regular source pin");
        assert_eq!(ubuntu_source.family, "Ubuntu");
        assert_eq!(
            ubuntu_source.sha256,
            "52c1afa489ae7bfd893af6cdd9f1af258005703600449e70d338caabcff507e5"
        );
        assert!(matches!(
            ubuntu_source.location,
            HintedSourceLocation::Ubuntu083Archive { .. }
        ));

        ubuntu.weight = 700;
        assert!(
            pinned_harness_source_for(&ubuntu).is_none(),
            "the regular source must not shadow an authored bold face"
        );
        ubuntu.weight = 400;
        ubuntu.italic = true;
        assert!(
            pinned_harness_source_for(&ubuntu).is_none(),
            "the regular source must not shadow an authored italic face"
        );
        ubuntu.italic = false;
        ubuntu.family = "Ubuntu Mono".into();
        assert!(pinned_harness_source_for(&ubuntu).is_none());

        // Arial is deliberately unpinned: see HINTED_HARNESS_SOURCES.
        let mut arial = request();
        arial.family = "Arial".into();
        assert!(pinned_harness_source_for(&arial).is_none());
    }

    #[test]
    fn task5_hinted_source_is_registered_before_existing_provider_bytes() {
        let mut options = ConvertOptions {
            font_bytes: vec![vec![2, 3]],
            font_hash_pins: vec!["existing".into()],
            ..ConvertOptions::default()
        };

        prepend_pinned_harness_font(&mut options, vec![1], "pinned");

        assert_eq!(options.font_bytes, vec![vec![1], vec![2, 3]]);
        assert_eq!(options.font_hash_pins, vec!["pinned", "existing"]);
    }

    #[test]
    fn provider_fetch_skips_an_alias_target_already_bundled_exactly() {
        let source = office_core::testkit::minimal_docx(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/></w:rPr><w:t>Bundled target</w:t></w:r></w:p>"#,
        );
        let requests = office_core::font_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .expect("preflight Calibri request");
        assert!(requests.iter().any(|request| request.family == "Calibri"));
        let (provider, satisfied) = provider_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
            &requests,
        )
        .expect("filter provider requests");
        assert!(
            provider.is_empty(),
            "Carlito is already bundled: {provider:?}"
        );
        assert_eq!(satisfied, BTreeSet::from(["Carlito".to_string()]));
    }

    #[test]
    fn provider_fetch_prefers_bundled_liberation_for_arial() {
        let source = office_core::testkit::minimal_docx(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/></w:rPr><w:t>Bundled substitute</w:t></w:r></w:p>"#,
        );
        let requests = office_core::font_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .expect("preflight Arial request");
        let (provider, satisfied) = provider_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
            &requests,
        )
        .expect("filter provider requests");

        assert!(
            provider.is_empty(),
            "Liberation Sans is bundled: {provider:?}"
        );
        assert_eq!(satisfied, BTreeSet::from(["Liberation Sans".to_string()]));
    }

    #[test]
    fn resolved_family_contract_records_liberation_instead_of_the_unused_alias() {
        let source = office_core::testkit::minimal_docx(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:t>Family contract</w:t></w:r></w:p>"#,
        );

        let resolved = resolve_for_case(
            "fixture",
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .expect("resolve bundled Arial substitute");
        // The unused Arimo alias must never be recorded. On machines with a
        // local Office install the real Arial also registers; the bundled
        // Liberation contract holds either way.
        assert!(resolved.demanded_families.contains("Liberation Sans"));
        assert!(!resolved.demanded_families.contains("Arimo"));
        assert!(
            resolved
                .demanded_families
                .iter()
                .all(|family| family == "Liberation Sans" || family == "Arial")
        );
    }

    #[test]
    fn bundled_arial_conversion_embeds_liberation_without_provider_fetches() {
        let source = office_core::testkit::minimal_docx(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial"/></w:rPr><w:t>Application-owned face</w:t></w:r></w:p>"#,
        );
        let requests = office_core::font_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .expect("preflight Arial request");
        let (provider, satisfied) = provider_requests(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
            &requests,
        )
        .expect("resolve bundled Arial face");
        assert!(provider.is_empty(), "provider must not fetch: {provider:?}");
        assert_eq!(satisfied, BTreeSet::from(["Liberation Sans".to_string()]));

        let output = office_core::convert(
            &source,
            office_core::OfficeFormat::Docx,
            &ConvertOptions::default(),
        )
        .expect("convert Arial through bundled Liberation");
        let embedded = crate::fonts::embedded_font_families(&output.pdf)
            .expect("read embedded output families");
        assert!(embedded.contains("Liberation Sans"), "{embedded:?}");
        assert!(!embedded.contains("Arimo"), "{embedded:?}");
    }
}
