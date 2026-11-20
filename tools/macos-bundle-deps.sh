#!/usr/bin/env bash
# Completes a macOS app bundle after macdeployqt: copies the Homebrew
# libraries that are still missing from Contents/Frameworks (macdeployqt does
# not follow @rpath references between non-Qt libraries, e.g. libwebp ->
# libsharpyuv) and lets every bundled library find its neighbours.
#
# Usage: tools/macos-bundle-deps.sh "dist/PDF to Word.app"
set -euo pipefail

APP="$1"
FW="$APP/Contents/Frameworks"
PREFIX="$(brew --prefix)"

find_lib() {
  local name="$1"
  for dir in "$PREFIX/lib" "$PREFIX"/opt/*/lib; do
    if [ -e "$dir/$name" ]; then
      echo "$dir/$name"
      return 0
    fi
  done
  return 1
}

has_rpath() {
  # Not grep -q: with pipefail an early exit would turn a match into a failure.
  otool -l "$1" | awk '/LC_RPATH/ {getline; getline; print $2}' | grep -x "$2" > /dev/null
}

changed=1
round=0
while [ "$changed" -eq 1 ] && [ "$round" -lt 10 ]; do
  changed=0
  round=$((round + 1))
  for f in "$FW"/*.dylib "$APP/Contents/MacOS/"*; do
    [ -f "$f" ] || continue
    for dep in $(otool -L "$f" | tail -n +2 | awk '{print $1}'); do
      case "$dep" in
        /usr/lib/* | /System/* | *.framework/*) continue ;;
      esac
      name="${dep##*/}"
      # The library's own install name.
      [ "$name" = "${f##*/}" ] && continue
      if [ ! -e "$FW/$name" ]; then
        if src="$(find_lib "$name")"; then
          echo "bundling $name (needed by ${f##*/})"
          cp -L "$src" "$FW/$name"
          chmod u+w "$FW/$name"
          install_name_tool -id "@rpath/$name" "$FW/$name"
          changed=1
        else
          echo "warning: $name (needed by ${f##*/}) not found" >&2
          continue
        fi
      fi
      # Absolute Homebrew paths point into the bundle instead.
      case "$dep" in
        "$PREFIX"/* | /usr/local/* | /opt/homebrew/*)
          chmod u+w "$f"
          install_name_tool -change "$dep" "@rpath/$name" "$f"
          ;;
      esac
    done
    # @rpath references resolve next to the library (libraries) or in
    # Frameworks (executables).
    case "$f" in
      "$FW"/*) want="@loader_path" ;;
      *) want="@executable_path/../Frameworks" ;;
    esac
    if ! has_rpath "$f" "$want"; then
      chmod u+w "$f"
      install_name_tool -add_rpath "$want" "$f"
    fi
  done
done
