#!/usr/bin/env bash
# Fetches the prebuilt pdfium dynamic library matching pdfium-render's default
# binding (chromium/7763) into pdf-core/pdfium/lib/.
set -euo pipefail

# Keep this release coupled to the pdfium-render binding. Fidelity inputs must
# never drift because a caller supplied a different build number.
PDFIUM_BUILD="7763"
cd "$(dirname "$0")/.."

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET="mac-arm64" ;;
  Darwin-x86_64) TARGET="mac-x64" ;;
  Linux-aarch64) TARGET="linux-arm64" ;;
  Linux-x86_64) TARGET="linux-x64" ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_BUILD}/pdfium-${TARGET}.tgz"
case "$TARGET" in
  mac-arm64) CHECKSUM_KEY="PDFIUM_SHA256_MAC_ARM64" ;;
  mac-x64) CHECKSUM_KEY="PDFIUM_SHA256_MAC_X64" ;;
  linux-arm64) CHECKSUM_KEY="PDFIUM_SHA256_LINUX_ARM64" ;;
  linux-x64) CHECKSUM_KEY="PDFIUM_SHA256_LINUX_X64" ;;
esac
EXPECTED_SHA256="${!CHECKSUM_KEY:-}"
if [[ ! "${EXPECTED_SHA256}" =~ ^[a-fA-F0-9]{64}$ ]]; then
  echo "owner-required checksum/provenance gate: set a trustworthy ${CHECKSUM_KEY} for ${URL}; refusing an unverified PDFium download" >&2
  exit 1
fi
EXPECTED_SHA256="$(printf '%s' "$EXPECTED_SHA256" | tr '[:upper:]' '[:lower:]')"
echo "fetching ${URL}"
mkdir -p pdfium
ARCHIVE="pdfium/.docyantra-pdfium-${TARGET}-$$.tgz"
if [[ -e "$ARCHIVE" ]]; then
  echo "refusing to overwrite existing temporary archive: $ARCHIVE" >&2
  exit 1
fi
trap 'rm -f -- "$ARCHIVE"' EXIT
curl --fail --location --silent --show-error --output "$ARCHIVE" "${URL}"
if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "${EXPECTED_SHA256}" "$ARCHIVE" | sha256sum --check --status
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
  [[ "$ACTUAL_SHA256" == "$EXPECTED_SHA256" ]] || {
    echo "PDFium SHA-256 mismatch: expected ${EXPECTED_SHA256}, got ${ACTUAL_SHA256}" >&2
    exit 1
  }
else
  echo "a SHA-256 tool (sha256sum or shasum) is required" >&2
  exit 1
fi
tar -xzf "$ARCHIVE" -C pdfium
echo "pdfium ready: $(ls pdfium/lib/)"
