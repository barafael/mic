#!/usr/bin/env bash
# mic's macOS release into $1: mic.app in a dmg, beside a link to Applications. The bundle's
# Info.plist makes it what opens mic:// links once it's in Applications, and asks for the
# microphone and the camera; screen sharing asks on its own. Signed ad hoc only: Gatekeeper warns
# on first start (right-click, Open). libvpx is linked statically (VPX_STATIC=1, Homebrew's).
#
# Needs rsvg-convert (Homebrew's librsvg) for the icon; iconutil and hdiutil are macOS's.
set -euo pipefail
out=$(cd "$(dirname "${1:?"usage: $0 <directory>"}")" && pwd)/$(basename "$1")
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

app=$stage/dmg/mic.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/mic"
# The icon, at every size an .icns has.
iconset=$stage/mic.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w $size -h $size packaging/$id.svg -o "$iconset/icon_${size}x${size}.png"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) packaging/$id.svg -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output "$app/Contents/Resources/mic.icns" "$iconset"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>mic</string>
  <key>CFBundleDisplayName</key><string>mic</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleExecutable</key><string>mic</string>
  <key>CFBundleIconFile</key><string>mic</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>mic sends your voice to the call you join.</string>
  <key>NSCameraUsageDescription</key><string>mic sends your camera to the call you join.</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>mic invite</string>
      <key>CFBundleURLSchemes</key><array><string>mic</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"
ln -s /Applications "$stage/dmg/Applications"
cp README.md CHANGELOG.md LICENSE-APACHE LICENSE-MIT "$stage/dmg/"
hdiutil create -volname mic -srcfolder "$stage/dmg" -ov -format UDZO \
  "$out/mic-$version-macos-$arch.dmg"
ls -l "$out"
