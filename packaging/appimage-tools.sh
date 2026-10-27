#!/usr/bin/env bash
# Fetches what packaging/linux.sh makes an AppImage with into $1, each pinned by its SHA-256:
# appimagetool, and the runtime it prepends, so that the AppImage isn't built with whatever was
# released last. Prints the variables linux.sh reads.
set -euo pipefail
into=${1:?"usage: $0 <directory>"}
mkdir -p "$into"
fetch() {
  local url=$1 sha256=$2 file=$into/${1##*/}
  if [ ! -f "$file" ] || ! echo "$sha256  $file" | sha256sum --check --status; then
    curl --fail --silent --show-error --location --output "$file" "$url"
  fi
  echo "$sha256  $file" | sha256sum --check --quiet
  chmod +x "$file"
}
fetch https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
  ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
fetch https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
  2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
echo "APPIMAGETOOL=$into/appimagetool-x86_64.AppImage"
echo "APPIMAGE_RUNTIME=$into/runtime-x86_64"
