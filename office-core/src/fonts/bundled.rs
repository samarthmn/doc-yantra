//! Access to the fonts embedded at build time by `build.rs`.
//!
//! With the `bundled-fonts` feature, `build.rs` scans `office-core/fonts/**` and
//! generates a `BUNDLED_FONTS` table; without the feature (or with an empty
//! `fonts/` directory) the table is empty.

#[cfg(feature = "bundled-fonts")]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/bundled_fonts.rs"));
}

/// The bundled fonts as `(family hint, sfnt bytes)` pairs. Empty when the
/// `bundled-fonts` feature is off or no fonts have been vendored yet.
pub fn bundled_fonts() -> &'static [(&'static str, &'static [u8])] {
    #[cfg(feature = "bundled-fonts")]
    {
        generated::BUNDLED_FONTS
    }
    #[cfg(not(feature = "bundled-fonts"))]
    {
        &[]
    }
}
