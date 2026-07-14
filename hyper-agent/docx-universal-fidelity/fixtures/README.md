# Plan fixtures — base64-encoded reference files

The six reference files for this plan (Google-Docs-produced DOCX 1–3 and their
Microsoft-Word-exported PDF ground truths) are committed here as **base64 text**:
the planning agent pushes through a text-only GitHub API surface, and base64
round-trips the bytes bit-exact. `4.docx`/`4.pdf` are NOT here — they are already
committed as `fidelity-corpus/docx/complex/`.

**If you hold the original files, ignore this directory** — human-tasks.md § H1
copies them straight into the corpus. This directory exists so anyone *without*
the originals (a coding agent on a fresh checkout, a teammate) can materialize
them from the repo alone:

```bash
# from the repo root
mkdir -p fidelity-corpus/docx/{gdocs-anchors-toc,gdocs-tables-longdoc,gdocs-columns}
python3 - <<'EOF'
import base64, pathlib
pairs = {
  '1.docx': 'gdocs-anchors-toc/source.docx',     '1.pdf': 'gdocs-anchors-toc/ground-truth.pdf',
  '2.docx': 'gdocs-tables-longdoc/source.docx',  '2.pdf': 'gdocs-tables-longdoc/ground-truth.pdf',
  '3.docx': 'gdocs-columns/source.docx',         '3.pdf': 'gdocs-columns/ground-truth.pdf',
}
src = pathlib.Path('hyper-agent/docx-universal-fidelity/fixtures')
for name, dest in pairs.items():
    out = pathlib.Path('fidelity-corpus/docx') / dest
    out.write_bytes(base64.b64decode((src / f'{name}.b64').read_text()))
    print('wrote', out)
EOF
```

After materializing (either way), verify the binaries' SHA-256 (`shasum -a 256`)
against the table below / H1. Do not commit decoded binaries under `hyper-agent/`.

| File | SHA-256 of DECODED binary | git blob SHA-1 of this .b64 file | .b64 length |
|---|---|---|---|
| `1.docx.b64` | `1e68879b7bd7b161ed320fa5f203cfe3709cb300a4c4476941bc2ea60b47ca91` | `7463f8b60d4bfc85e98ea0b12aec69ecee6e5e16` | 42388 |
| `1.pdf.b64` | `8b42f556073a3a2e251664abc6a65ac247975124072d658a2920406bee2533c1` | `b6e35ea71f45cfe20ff39efdeee73491ff52e922` | 380688 |
| `2.docx.b64` | `5ea1a2c4e049adf55e2618ac7a73f72791170a81dd1bee2b6a888f736d3287f3` | `4ebde7de20573bbd7c75e04e1f024bd8344328d1` | 72880 |
| `2.pdf.b64` | `0edeb20b010ed04474eee8add27b58476cc384b584bf0abfcfbc2fe983323ac7` | `a8560490a82036f9308c98676cb428b9dc4982d8` | 1026664 |
| `3.docx.b64` | `4fcfc62f878667f0f1cbd5f55204a7cf4e3c88dd31c05354e0bd54651ef5a397` | `1aed9a8742adacc450db54a417b8fc9ce5f45e88` | 14692 |
| `3.pdf.b64` | `5f731bda8c149c00a1683f81f15049babab9d10366fae38d0f19f8164956d6f0` | `be86b97687956e74c748a5c362f1fcc4d604200d` | 319208 |

Originals also live on the planning thread:
https://hyperagent.com/thread/cmrki1y8p0ixg06adbyatevrz
