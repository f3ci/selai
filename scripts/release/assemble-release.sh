#!/usr/bin/env bash
# Gathers the files for a GitHub release from the downloaded workflow artifacts.
# Each platform's SHA256SUMS is checked first, so a corrupted download stops the
# release. Writes one SHA256SUMS over everything that will be published.
#
# Usage: scripts/release/assemble-release.sh <artifacts-dir> <dist-dir>
#   <artifacts-dir> holds installers-linux/, installers-windows/,
#   installers-macos/, installers-macos-intel/ and sbom/, as
#   actions/download-artifact writes them.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifacts-dir> <dist-dir>" >&2
  exit 2
fi
src=$1
dist=$2
mkdir -p "$dist"

for platform in linux windows macos macos-intel; do
  dir="$src/installers-$platform"
  if [[ ! -f "$dir/SHA256SUMS" ]]; then
    echo "missing $dir/SHA256SUMS" >&2
    exit 1
  fi
  (cd "$dir" && sha256sum -c SHA256SUMS)
  for f in "$dir"/*; do
    name=$(basename "$f")
    [[ "$name" == "SHA256SUMS" ]] && continue
    if [[ -e "$dist/$name" ]]; then
      echo "two artifacts are both named $name" >&2
      exit 1
    fi
    cp "$f" "$dist/"
  done
done

# SBOMs are published as one archive, so the release page lists installers first.
if [[ -d "$src/sbom" ]]; then
  tar -C "$src/sbom" -czf "$dist/sbom-cyclonedx.tar.gz" .
fi

(cd "$dist" && rm -f SHA256SUMS && sha256sum -- * > SHA256SUMS && sha256sum -c SHA256SUMS)
echo "Release files:"
ls -l "$dist"
