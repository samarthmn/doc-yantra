//! Global pdfium binding with a crate-wide serialization lock.
//!
//! pdfium is NOT thread-safe, and pdfium-render 0.9.2's `thread_safe` feature
//! only adds `Send`/`Sync` markers — it does NOT serialize FFI calls
//! (concurrent use segfaults; reproduced by three modules independently).
//! All pdfium work must therefore go through [`lock()`], which hands out a
//! [`PdfiumHandle`] holding a process-wide mutex guard. Documents loaded via
//! the handle borrow from it, so the type system prevents any pdfium object
//! from outliving the lock.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pdfium_render::prelude::*;

use crate::error::{PdfCoreError, Result};

static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();
static FFI_LOCK: Mutex<()> = Mutex::new(());

fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var("PDFIUM_DYNAMIC_LIB_PATH") {
        dirs.push(PathBuf::from(dir));
    }
    // Crate-local prebuilt (fetched by scripts/fetch-pdfium.sh)
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("pdfium/lib"));
    dirs
}

fn bind() -> Option<Pdfium> {
    for dir in candidate_dirs() {
        let lib = Pdfium::pdfium_platform_library_name_at_path(&dir);
        if let Ok(bindings) = Pdfium::bind_to_library(lib) {
            return Some(Pdfium::new(bindings));
        }
    }
    // Fall back to a system-installed pdfium, if any.
    Pdfium::bind_to_system_library().ok().map(Pdfium::new)
}

/// Exclusive access to the process-wide pdfium instance. Derefs to [`Pdfium`];
/// everything borrowed through it (documents, pages, bitmaps) is lifetime-bound
/// to this handle and therefore to the lock.
pub struct PdfiumHandle {
    _guard: MutexGuard<'static, ()>,
    pdfium: &'static Pdfium,
}

impl Deref for PdfiumHandle {
    type Target = Pdfium;

    fn deref(&self) -> &Pdfium {
        self.pdfium
    }
}

/// Acquires the pdfium lock, binding the library on first use. Blocks while
/// another thread holds the handle. Poisoning is ignored (pdfium state is
/// process-global and a panicked holder leaves no partial state we track).
pub fn lock() -> Result<PdfiumHandle> {
    let guard = FFI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pdfium = PDFIUM.get_or_init(bind).as_ref().ok_or_else(|| {
        PdfCoreError::Pdfium(
            "could not load libpdfium; run scripts/fetch-pdfium.sh or set PDFIUM_DYNAMIC_LIB_PATH"
                .to_string(),
        )
    })?;
    Ok(PdfiumHandle {
        _guard: guard,
        pdfium,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::sample_pdf;

    #[test]
    fn binds_and_loads_a_document() {
        let pdfium = lock().expect("pdfium should bind to crate-local dylib");
        let bytes = sample_pdf(&["Hello pdfium"]);
        let doc = pdfium
            .load_pdf_from_byte_slice(&bytes, None)
            .expect("load sample pdf");
        assert_eq!(doc.pages().len(), 1);
    }
}
