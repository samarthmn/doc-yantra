# Pending DOCX fidelity cases

Cases staged here are **not** scanned by `fidelity -- run` (it only reads
`fidelity-corpus/docx/`), so incomplete cases cannot break the corpus gate.

## Adding the Word ground truth

For each case directory:

1. Open its `source.docx` (exactly these bytes — `input_sha256` pins them) in
   Microsoft Word.
2. Export to PDF (File → Save As / Export, PDF, default page setup, no
   scaling), and save it as `ground-truth.pdf` in the same case directory.
3. Finalization (done in-repo, scriptable): fill `word_version` (the PDF's
   Producer metadata), `ground_truth_exported_at` (the PDF CreationDate,
   YYYY-MM-DD), `golden_sha256` (SHA-256 of the PDF), run the case once to
   measure SSIM and set the frozen `ssim_threshold` and `expected_fonts`, then
   move the directory into `fidelity-corpus/docx/`.

The `Not recorded` placeholders are deliberate: the harness's provenance
validation rejects them, so a half-finished case can never pass silently.
