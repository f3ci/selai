#!/usr/bin/env bash
# Copies the installers produced by `pnpm tauri build` into a staging directory
# and writes SHA256SUMS for them. Linux and macOS only; Windows uses
# collect-bundles.ps1.
#
# Usage: scripts/release/collect-bundles.sh <bundle-dir> <stage-dir> [macos-arch]
#   <bundle-dir> is the Tauri bundle output, e.g. target/release/bundle
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <bundle-dir> <stage-dir> [macos-arch]" >&2
  exit 2
fi
bundle_dir=$1
stage_dir=$2
macos_arch=${3:-}
case "$macos_arch" in
  ""|aarch64|x64) ;;
  *) echo "unsupported macOS package architecture: $macos_arch" >&2; exit 2 ;;
esac

if [[ ! -d "$bundle_dir" ]]; then
  echo "bundle dir not found: $bundle_dir" >&2
  exit 1
fi

mkdir -p "$stage_dir"
shopt -s nullglob

installers=(
  "$bundle_dir"/deb/*.deb
  "$bundle_dir"/appimage/*.AppImage
  "$bundle_dir"/dmg/*.dmg
)
for f in "${installers[@]}"; do
  cp -f "$f" "$stage_dir/"
done

# A macOS .app is a directory. Zip it so it is one uploadable, checksummable file.
if command -v ditto >/dev/null 2>&1; then
  for app in "$bundle_dir"/macos/*.app; do
    archive_name=$(basename "$app")
    if [[ -n "$macos_arch" ]]; then
      archive_name="${archive_name%.app}_${macos_arch}.app"
    fi
    ditto -c -k --sequesterRsrc --keepParent "$app" "$stage_dir/$archive_name.zip"
  done
fi

# Only SHA256SUMS may be left in the stage dir from an earlier run.
rm -f "$stage_dir/SHA256SUMS"
cd "$stage_dir"
files=(*)
if (( ${#files[@]} == 0 )); then
  echo "no installers found under $bundle_dir" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  hash_cmd=(sha256sum)
else
  hash_cmd=(shasum -a 256)
fi
"${hash_cmd[@]}" "${files[@]}" > SHA256SUMS
"${hash_cmd[@]}" -c SHA256SUMS

echo "Staged ${#files[@]} file(s) in $stage_dir:"
cat SHA256SUMS
