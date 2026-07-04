#!/usr/bin/env bash
# Fetches the prebuilt pdfium dynamic library matching pdfium-render's default
# binding (chromium/7763) into pdf-core/pdfium/lib/.
set -euo pipefail

PDFIUM_BUILD="${PDFIUM_BUILD:-7763}"
cd "$(dirname "$0")/.."

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET="mac-arm64" ;;
  Darwin-x86_64) TARGET="mac-x64" ;;
  Linux-aarch64) TARGET="linux-arm64" ;;
  Linux-x86_64) TARGET="linux-x64" ;;
  *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_BUILD}/pdfium-${TARGET}.tgz"
echo "fetching ${URL}"
mkdir -p pdfium
curl -sSL "${URL}" | tar -xz -C pdfium
echo "pdfium ready: $(ls pdfium/lib/)"
