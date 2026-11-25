#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
vendor_root="$repo_root/.cargo/rune/vendor"
revision="ff8c5bfa1aa2458219c0a5e91ebc98cb34400e39"
temp_root=$(mktemp -d)
trap 'rm -rf "$temp_root"' EXIT

archive="$temp_root/rune.tar.gz"
unpacked="$temp_root/rune"
mkdir -p "$unpacked"
curl --fail --location --silent --show-error "https://github.com/anomalyco/rune/archive/$revision.tar.gz" --output "$archive"
tar -xzf "$archive" -C "$unpacked"
rune_root=$(find "$unpacked" -mindepth 1 -maxdepth 1 -type d | head -n 1)
section=$(sed -n '/^\[patch\.crates-io\]/,/^\[/p' "$rune_root/Cargo.toml")
if test -z "$section"; then
    printf 'Rune manifest has no patch section\n' >&2
    exit 1
fi

mkdir -p "$vendor_root"
printf '[patch.crates-io]\n' > "$repo_root/.cargo/rune/patches.toml"
printf '%s\n' "$section" | sed -n 's/^\([A-Za-z0-9_-]*\) = { path = "\([^"]*\)" }/\1 \2/p' |
while read -r crate_name source_path; do
    test -n "$crate_name"
    source="$rune_root/$source_path"
    destination="$vendor_root/$crate_name"
    test -f "$source/Cargo.toml"
    rm -rf "$destination"
    mkdir -p "$destination"
    cp -R "$source/." "$destination/"
    printf '%s = { path = ".cargo/rune/vendor/%s" }\n' "$crate_name" "$crate_name" >> "$repo_root/.cargo/rune/patches.toml"
    printf 'Vendored %s\n' "$crate_name"
done

printf 'Rune vendor patches are ready.\n'
