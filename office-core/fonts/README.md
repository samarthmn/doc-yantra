# Bundled fallback fonts (vendored by a human — plan task H2)

This directory holds the metric-compatible OFL fonts the engine embeds via the
`bundled-fonts` feature (`include_bytes!`). **Agents must not fetch these
binaries from the network** — see `hyper-agent/office-engine/human-tasks.md` H2.

Expected layout once vendored (20 TTFs + 3 licenses, all SIL OFL 1.1):

```
fonts/
  liberation/  Liberation{Sans,Serif,Mono}-{Regular,Bold,Italic,BoldItalic}.ttf + LICENSE
  carlito/     Carlito-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Calibri metric-compatible
  caladea/     Caladea-{Regular,Bold,Italic,BoldItalic}.ttf + OFL.txt   # Cambria metric-compatible
```

The font database loader (agent task 2) globs whatever `.ttf` files are present,
so the crate compiles and all non-font tests pass with this directory empty; the
bundled-font matching tests and all fidelity scoring stay red until the fonts are
committed here.
