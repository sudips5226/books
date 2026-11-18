#!/usr/bin/env bash
# Downloads a prebuilt PDFium library (https://github.com/bblanchon/pdfium-binaries)
# matching the API version the converter is built against.
#
# Usage: tools/fetch-pdfium.sh [DEST_DIR] [PLATFORM]
#   DEST_DIR  where libpdfium.so / libpdfium.dylib / pdfium.dll is placed (default: ./pdfium)
#   PLATFORM  linux-x64 | linux-arm64 | mac-x64 | mac-arm64 | win-x64 | win-arm64 (auto-detected)
set -euo pipefail

PDFIUM_BUILD="${PDFIUM_BUILD:-7350}"
DEST="${1:-pdfium}"
PLATFORM="${2:-}"

if [[ -z "$PLATFORM" ]]; then
  os="$(uname -s)"; arch="$(uname -m)"
  case "$arch" in x86_64|amd64) arch=x64 ;; aarch64|arm64) arch=arm64 ;; *) echo "unsupported arch $arch" >&2; exit 1 ;; esac
  case "$os" in
    Linux) PLATFORM="linux-$arch" ;;
    Darwin) PLATFORM="mac-$arch" ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM="win-$arch" ;;
    *) echo "unsupported OS $os" >&2; exit 1 ;;
  esac
fi

url="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_BUILD}/pdfium-${PLATFORM}.tgz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $url"
curl -fsSL "$url" -o "$tmp/pdfium.tgz"
tar -xzf "$tmp/pdfium.tgz" -C "$tmp"
mkdir -p "$DEST"
case "$PLATFORM" in
  win-*) cp "$tmp/bin/pdfium.dll" "$DEST/" ;;
  mac-*) cp "$tmp/lib/libpdfium.dylib" "$DEST/" ;;
  *) cp "$tmp/lib/libpdfium.so" "$DEST/" ;;
esac
echo "PDFium $(grep -E '^(MAJOR|BUILD)=' "$tmp/VERSION" | tr '\n' ' ')installed to $DEST"
echo "Run with: PDFIUM_DYNAMIC_LIB_PATH=$DEST <program>   (or copy the library next to the executable)"
