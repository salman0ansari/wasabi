#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_dir=$(cd -- "$script_dir/.." && pwd)

expected=$(tr -d '[:space:]' < "$repository_dir/VERSION")
root_version=$(awk '/^\[workspace.package\]/{found=1; next} found && /^version = /{gsub(/[" ]/, "", $3); print $3; exit}' "$repository_dir/Cargo.toml")
desktop_version=$(awk '/^\[package\]/{found=1; next} found && /^version = /{gsub(/[" ]/, "", $3); print $3; exit}' "$repository_dir/apps/desktop/Cargo.toml")

if [[ -z "$expected" || -z "$root_version" || -z "$desktop_version" ]]; then
    echo "release metadata is incomplete" >&2
    exit 1
fi

if [[ "$expected" != "$root_version" || "$expected" != "$desktop_version" ]]; then
    echo "version mismatch: VERSION=$expected root=$root_version desktop=$desktop_version" >&2
    exit 1
fi

if ! grep -Fq "## [$expected]" "$repository_dir/CHANGELOG.md"; then
    echo "CHANGELOG.md has no release heading for $expected" >&2
    exit 1
fi

echo "Wasabi release metadata is consistent: $expected"
