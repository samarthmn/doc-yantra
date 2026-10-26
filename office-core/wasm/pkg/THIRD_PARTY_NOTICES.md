# Third-party notices

DocYantra first-party source and wrapper code is distributed under the
AGPL-3.0-only license in [`LICENSE`](LICENSE). This notice does not change the
terms of any third-party component.

The following bundled or redistributed materials retain their upstream terms:

* PDF CMap resources retain Adobe's permissive CMap redistribution terms; the
  notice is in [`pdf-core/assets/cmaps/LICENSE-adobe-cmap-resources.md`](pdf-core/assets/cmaps/LICENSE-adobe-cmap-resources.md).
* PDFium retains its upstream BSD-style license and notices; see
  [`pdf-core/pdfium/LICENSE`](pdf-core/pdfium/LICENSE) and the accompanying
  `licenses/` directory.
* Distribution fonts: each font's upstream license is shipped beside the font
  in [`packages/doc-yantra-fonts/fonts`](packages/doc-yantra-fonts/fonts) and is
  listed in that package's `manifest.json`.
* Native-development Office fonts retain their upstream terms; the Caladea,
  Carlito, and Liberation licenses are shipped with their bytes under
  [`office-core/fonts`](office-core/fonts).
* Rust and JavaScript dependencies retain their own licenses as declared by
  their upstream packages and lockfiles.

Redistributions must preserve this notice and the applicable upstream notices.
