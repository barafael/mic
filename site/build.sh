#!/bin/sh
# Builds the site (D25) into $1: its pages as they are, and version.json, which mic checks at start
# (D15, design §5.12), from the workspace's version. $MIC_SITE_LATEST overrides the version, as the
# box's site does to say a newer one is out.
set -eu
out=$1
here=$(dirname "$0")
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../Cargo.toml" | head -1)
latest=${MIC_SITE_LATEST:-$version}
[ -n "$latest" ] || { echo "site/build.sh: no version in Cargo.toml" >&2; exit 1; }
mkdir -p "$out/j"
cp "$here/index.html" "$here/style.css" "$here/invite.js" "$out/"
cp "$here/j/index.html" "$out/j/"
# The major is the wire format (§5.12): a client of another major can't share a room. Before 1.0,
# every minor is one.
case $latest in
    0.*) min_compatible=$(echo "$latest" | sed 's/^\(0\.[0-9]*\)\..*/\1.0/') ;;
    *) min_compatible=$(echo "$latest" | sed 's/^\([0-9]*\)\..*/\1.0.0/') ;;
esac
cat > "$out/version.json" <<JSON
{
  "latest": "$latest",
  "min_compatible": "$min_compatible",
  "download": "/#download"
}
JSON
