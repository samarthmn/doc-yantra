# Native/dev fallback fonts

This directory holds metric-compatible OFL fonts embedded only when the
`bundled-fonts` feature is enabled. Native default builds use them to keep Rust
tests and the fidelity harness hermetic. The production wasm build disables the
feature; no file here may appear in the packed Office npm artifact.

Expected layout once vendored (20 TTFs + 3 licenses, all SIL OFL 1.1):

```
fonts/
  liberation/  Liberation{Sans,Serif,Mono}-{Regular,Bold,Italic,BoldItalic}.ttf + LICENSE
  carlito/     Carlito-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Calibri metric-compatible
  caladea/     Caladea-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Cambria metric-compatible
```

Runtime/browser/server consumers obtain fonts through a host-authorized
`FontProvider`, app-owned boot assets, enterprise repositories, uploads, or an
explicit system-font directory. Validated registration enforces identity,
SHA-256, coverage, variable axes, size, and OS/2 embedding permission.
