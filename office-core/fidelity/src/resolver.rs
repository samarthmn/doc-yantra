//! Locked, content-addressed font resolution for fidelity runs.

use anyhow::{Context, Result};
use fidelity::evidence::{is_sha256, sha256_hex};
use office_core::fonts::{FontSubstitutionClass, classify_font_request};
use office_core::{ConvertOptions, FontExpectation, FontRequest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const CDN_BASE: &str = "https://cdn.jsdelivr.net/fontsource/fonts/";
const MAX_FONT_BYTES: u64 = 10 * 1024 * 1024;

const ALIASES: &[(&str, &str)] = &[
    ("arial", "arimo"),
    ("arimo", "arimo"),
    ("caladea", "caladea"),
    ("calibri", "carlito"),
    ("calibri light", "carlito"),
    ("cambria", "caladea"),
    ("carlito", "carlito"),
    ("cousine", "cousine"),
    ("courier", "cousine"),
    ("courier new", "cousine"),
    ("helvetica", "arimo"),
    ("georgia", "gelasio"),
    ("liberation mono", "cousine"),
    ("liberation sans", "arimo"),
    ("liberation serif", "tinos"),
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts-cache")
}

pub(crate) fn resolve_for_case(
    case_name: &str,
    source: &[u8],
    base_options: &ConvertOptions,
) -> Result<ResolvedFonts> {
    let lock_path = lock_path();
    let lock_contents = fs::read_to_string(&lock_path)
        .with_context(|| format!("read fidelity font lock {}", lock_path.display()))?;
    let lock: FontLock = toml::from_str(&lock_contents)
        .with_context(|| format!("parse fidelity font lock {}", lock_path.display()))?;
    let requests =
        office_core::font_requests(source, office_core::OfficeFormat::Docx, base_options)
            .with_context(|| format!("discover fonts for fidelity case {case_name}"))?;
    let (provider_requests, satisfied_families) =
        provider_requests(source, base_options, &requests)?;
    let fetcher = HttpFetcher::new()?;
    let mut resolved = resolve_requests_with_fetch(
        case_name,
        &provider_requests,
        &lock,
        &cache_dir(),
        base_options,
        |url| fetcher.fetch(url),
    )?;
    resolved.families.extend(satisfied_families);
    resolved.demanded_families = resolved.families.clone();
    Ok(resolved)
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
        let (requests, _) = provider_requests(source, &ConvertOptions::default(), &requests)?;
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
            .is_some_and(|length| length > MAX_FONT_BYTES)
        {
            anyhow::bail!("font response exceeds {MAX_FONT_BYTES} bytes: {url}");
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(MAX_FONT_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read {url}"))?;
        if bytes.len() as u64 > MAX_FONT_BYTES {
            anyhow::bail!("font response exceeds {MAX_FONT_BYTES} bytes: {url}");
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
    base_options: &ConvertOptions,
    requests: &[FontRequest],
) -> Result<(Vec<FontRequest>, BTreeSet<String>)> {
    let document = office_core::layout::WordDoc::open(source, base_options)
        .context("open document for fidelity provider resolution")?;
    let mut provider = Vec::new();
    let mut satisfied = BTreeSet::new();
    for request in requests {
        let Some(id) = mapped_id(request) else {
            continue;
        };
        let native = office_core::fonts::resolve_exact_or_substitute_with_stretch(
            &document.fonts,
            &request.family,
            request.weight,
            request.italic,
            request.stretch,
        );
        if let Some(face) = native.and_then(|face| document.fonts.face(face)) {
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
        let (provider, satisfied) =
            provider_requests(&source, &ConvertOptions::default(), &requests)
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
        let (provider, satisfied) =
            provider_requests(&source, &ConvertOptions::default(), &requests)
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

        let resolved = resolve_for_case("fixture", &source, &ConvertOptions::default())
            .expect("resolve bundled Arial substitute");
        assert_eq!(
            resolved.demanded_families,
            BTreeSet::from(["Liberation Sans".to_string()])
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
        let (provider, satisfied) =
            provider_requests(&source, &ConvertOptions::default(), &requests)
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
