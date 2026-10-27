#!/usr/bin/env bash
# mic's Linux release into $1: a tarball of the binary, its desktop file and icon, and an AppImage
# of the same. The binary links libvpx statically where VPX_STATIC=1 finds a static one (CI);
# otherwise the AppImage carries the libvpx it links, and the tarball wants the system's. The rest
# (Vulkan, X11 or Wayland, ALSA, fontconfig) is the system's, as a desktop has them.
#
# The AppImage takes appimagetool and its runtime from $APPIMAGETOOL and $APPIMAGE_RUNTIME
# (packaging/appimage-tools.sh fetches both, pinned); without them, it leaves the AppDir.
set -euo pipefail
out=$(realpath -m "${1:?"usage: $0 <directory>"}")
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
version=$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "mic-slint"))')
arch=$(uname -m)
id=io.github.barafael.mic
cargo build --release --locked -p mic-slint
binary=target/release/mic
"$binary" --version
mkdir -p "$out"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# The tarball: unpack anywhere, run ./mic, or `./mic --register` for links.
name=mic-$version-linux-$arch
mkdir -p "$stage/$name"
cp "$binary" packaging/$id.desktop packaging/$id.svg README.md CHANGELOG.md LICENSE-APACHE \
  LICENSE-MIT "$stage/$name/"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"

# The AppImage: one file; `mic.AppImage --register` makes it what opens links.
appdir=$stage/AppDir
mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/scalable/apps"
cp "$binary" "$appdir/usr/bin/mic"
cp packaging/$id.desktop "$appdir/usr/share/applications/"
cp packaging/$id.svg "$appdir/usr/share/icons/hicolor/scalable/apps/"
cp packaging/$id.desktop packaging/$id.svg "$appdir/"
ln -s $id.svg "$appdir/.DirIcon"
# A libvpx linked dynamically goes along.
if vpx=$(ldd "$binary" | awk '/libvpx/ { print $3 }') && [ -n "$vpx" ]; then
  cp -L "$vpx" "$appdir/usr/lib/"
fi
cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$here/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$here/usr/bin/mic" "$@"
APPRUN
chmod +x "$appdir/AppRun"
if [ -n "${APPIMAGETOOL:-}" ]; then
  ARCH=$arch "$APPIMAGETOOL" --appimage-extract-and-run --no-appstream \
    --runtime-file "${APPIMAGE_RUNTIME:?"set APPIMAGE_RUNTIME: packaging/appimage-tools.sh"}" \
    "$appdir" "$out/mic-$version-$arch.AppImage"
else
  # Without appimagetool, the AppDir as it would be packed, to look at or run (AppRun).
  rm -rf "$out/AppDir"
  cp -a "$appdir" "$out/AppDir"
  echo "no APPIMAGETOOL (packaging/appimage-tools.sh): no AppImage; the AppDir is in $out/AppDir"
fi
ls -l "$out"
