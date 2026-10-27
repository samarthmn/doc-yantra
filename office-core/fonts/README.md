# Native/dev fallback fonts

This directory holds native-only metric-compatible OFL fixtures embedded when
the `bundled-fonts` feature is enabled. Native default builds combine them with
the five shared Liberation faces owned by `packages/doc-yantra-fonts`; keeping
those shared files there avoids a second repository copy. The production wasm
build disables the feature, and no font may appear in the Office npm artifact.

Expected local-only layout (15 TTFs + 3 licenses, all SIL OFL 1.1):

```
fonts/
  liberation/  LiberationSerif-{Regular,Bold,Italic,BoldItalic}.ttf,
               LiberationMono-{Bold,Italic,BoldItalic}.ttf + LICENSE
  carlito/     Carlito-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Calibri metric-compatible
  caladea/     Caladea-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Cambria metric-compatible
```

The shared Liberation files and all runtime distribution fonts live in the
optional offline package. Runtime/browser/server consumers obtain exact fonts
through its validated local-first provider or the pinned first-party asset host.
