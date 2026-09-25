#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
status=0
scratch="$(mktemp -d -t tidepool-suite-check.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT

cargo metadata --no-deps --format-version 1 >"$scratch/metadata.json"
jq -r '
  .workspace_members as $members | .packages[]
  | select(.id as $id | $members | index($id))
  | [.name, .manifest_path] | @tsv
' "$scratch/metadata.json" >"$scratch/packages"
mapfile -t workspace_packages <"$scratch/packages"

for package in "${workspace_packages[@]}"; do
  IFS=$'\t' read -r crate manifest <<<"$package"
  crate_dir="$(dirname "$manifest")"
  suite_dir="$crate_dir/tests/suites"
  [[ -d "$suite_dir" ]] || continue
  # Cargo owns the binary list; every suite entry point must be registered.
  jq -r --arg crate "$crate" '.packages[] | select(.name == $crate) |
    .targets[] | select(.kind == ["test"]) | .src_path' "$scratch/metadata.json" \
    | while IFS= read -r source; do
        if [[ "$(dirname "$source")" == "$suite_dir" ]]; then basename "$source"; fi
      done | sort >"$scratch/registered"
  find "$suite_dir" -maxdepth 1 -type f -name '*.rs' -printf '%f\n' | sort >"$scratch/roots"
  if ! diff -u "$scratch/roots" "$scratch/registered"; then
    echo "error: $crate suite entry points must be registered exactly once in Cargo.toml" >&2
    status=1
  fi
  # autotests=false makes registration explicit. Do not silently lose a new
  # tests/*.rs file: every leaf must appear exactly once in a suite or as a
  # standalone Cargo target. Suite roots use literal #[path = "../name.rs"].
  sources="$scratch/$crate.sources"
  {
    jq -r --arg crate "$crate" '.packages[] | select(.name == $crate) |
      .targets[] | select(.kind == ["test"]) | .src_path' "$scratch/metadata.json" \
      | while IFS= read -r source; do
          if [[ "$(dirname "$source")" == "$crate_dir/tests" ]]; then
            basename "$source"
          elif [[ "$(dirname "$source")" == "$crate_dir/tests/suites" ]]; then
            sed -n 's/^#\[path = "\.\.\/\([^/]*\.rs\)"\]$/\1/p' "$source"
          else
            printf '%s\n' "${source#"$crate_dir/tests/"}"
          fi
        done
  } | sort >"$sources"
  {
    find "$crate_dir/tests" -maxdepth 1 -type f -name '*.rs' -printf '%f\n'
    find "$crate_dir/tests" -mindepth 2 -maxdepth 2 -type f -name main.rs -printf '%P\n'
  } | sort >"$scratch/leaves"
  if ! diff -u "$scratch/leaves" "$sources"; then
    echo "error: $crate test files must be registered exactly once" >&2
    status=1
  fi
done

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi

echo "Cargo suites register every integration test file exactly once"
