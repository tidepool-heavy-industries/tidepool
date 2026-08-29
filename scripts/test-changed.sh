#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
base="${1:-HEAD}"

mapfile -t changed < <(
  {
    git diff --name-only "$base"
    git ls-files --others --exclude-standard
  } | sort -u
)

if [[ "${#changed[@]}" -eq 0 ]]; then
  echo "no files changed relative to $base"
  exit 0
fi

echo "==> changed-file inner loop relative to $base"
printf '  %s\n' "${changed[@]}"

code_changed=0
haskell_changed=0
declare -A crates=()

mapfile -t workspace_crates < <(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.source == null) | [.name, .manifest_path] | @tsv')

for path in "${changed[@]}"; do
  case "$path" in
    *.rs|Cargo.toml|Cargo.lock|flake.nix|rust-toolchain.toml|.cargo/*|.config/*|scripts/*|justfile|dev/*)
      code_changed=1
      ;;
  esac
  case "$path" in
    haskell/src/*|haskell/app/*|haskell/lib/*|haskell/test/*.hs|haskell/*.cabal|haskell/cabal.project)
      haskell_changed=1
      ;;
  esac

  for entry in "${workspace_crates[@]}"; do
    IFS=$'\t' read -r name manifest_path <<<"$entry"
    crate_dir="${manifest_path%/Cargo.toml}"
    crate_rel="${crate_dir#"$PWD"/}"
    if [[ "$path" == "$crate_rel"/* ]]; then
      crates["$name"]=1
    fi
  done
done

if [[ "$code_changed" -eq 1 || "$haskell_changed" -eq 1 ]]; then
  source scripts/lib-extract.sh
  prepare_battery_artifacts changed just changed "$base"
  cleanup_exit() {
    local status=$?
    finalize_battery_artifacts "$status"
    return "$status"
  }
  trap cleanup_exit EXIT
  exec > >(tee -a "$BATTERY_NEXTEST_LOG") 2>&1
fi

if [[ "$code_changed" -eq 1 ]]; then
  resolve_tidepool_extract
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  scripts/test-suite-check.sh
  cargo nextest run --status-level fail --final-status-level fail
fi

heavy=' tidepool-runtime tidepool-repl tidepool-mcp tidepool-handlers tidepool-harness tidepool-testing '
for crate in "${!crates[@]}"; do
  if [[ "$heavy" == *" $crate "* ]]; then
    scripts/battery.sh -p "$crate" --lib
    if jq -e --arg crate "$crate" 'has($crate)' dev/test-suites.json >/dev/null; then
      echo "note: broader coverage is available with: just suite $crate"
    fi
  else
    cargo nextest run -p "$crate" --status-level fail --final-status-level fail
  fi
done

if [[ "$haskell_changed" -eq 1 ]]; then
  scripts/fixtures.sh check
  scripts/battery.sh -p tidepool-runtime \
    -E 'binary(jit_surface) or binary(user_library) or binary(cross_mode_targeted)'
fi

if [[ "$code_changed" -eq 0 && "$haskell_changed" -eq 0 ]]; then
  echo "documentation-only change: no executable checks selected"
fi

echo "changed-file checks passed (inner-loop selection; use 'just verify' before review)"
