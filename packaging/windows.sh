#!/usr/bin/env bash
# mic's Windows release into $1: a zip of mic.exe, with the README, changelog and licenses.
# Unpack anywhere; `mic.exe --register` makes it what opens mic:// links, for this user.
# libvpx is linked statically (vcpkg's, VPX_STATIC=1, as CI sets it up). Unsigned: SmartScreen
# warns on first start (More info, Run anyway). Runs in Git Bash, as GitHub's runners have it.
set -euo pipefail
out=$(realpath -m "${1:?"usage: $0 <directory>"}")
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
version=$(cargo metadata --format-version 1 --no-deps \
  | python -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "mic-slint"))')
cargo build --release --locked -p mic-slint
binary=target/release/mic.exe
"$binary" --version
mkdir -p "$out"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
name=mic-$version-windows-x86_64
mkdir -p "$stage/$name"
cp "$binary" README.md CHANGELOG.md LICENSE-APACHE LICENSE-MIT "$stage/$name/"
(cd "$stage" && 7z a -tzip "$out/$name.zip" "$name" >/dev/null)
ls -l "$out"
