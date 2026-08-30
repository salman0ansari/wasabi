#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_dir=$(cd -- "$script_dir/.." && pwd)
desktop_entry="$repository_dir/packaging/linux/wasabi.desktop"
desktop_manifest="$repository_dir/apps/desktop/Cargo.toml"

fail() {
    echo "linux packaging check failed: $*" >&2
    exit 1
}

[[ -f "$desktop_entry" ]] || fail "missing packaging/linux/wasabi.desktop"

binary_name=$(awk -F'"' '
    /^\[package\]$/ { in_package=1; next }
    /^\[/ && in_package { exit }
    in_package && /^name = / { print $2; exit }
' "$desktop_manifest")

[[ -n "$binary_name" ]] || fail "could not read desktop package name"

desktop_value() {
    local key=$1
    awk -F= -v key="$key" '$1 == key { sub(/^[^=]*=/, ""); print; exit }' "$desktop_entry"
}

[[ "$(desktop_value Type)" == "Application" ]] || fail "Type must be Application"
[[ "$(desktop_value Exec)" == "$binary_name" ]] || fail "Exec must match Cargo binary '$binary_name'"
[[ "$(desktop_value TryExec)" == "$binary_name" ]] || fail "TryExec must match Cargo binary '$binary_name'"
[[ "$(desktop_value Terminal)" == "false" ]] || fail "Terminal must be false"

categories=";$(desktop_value Categories);"
[[ "$categories" == *";Network;"* ]] || fail "Categories must include Network"
[[ "$categories" == *";InstantMessaging;"* ]] || fail "Categories must include InstantMessaging"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop_entry"
fi

echo "Wasabi Linux desktop entry is consistent with binary: $binary_name"
