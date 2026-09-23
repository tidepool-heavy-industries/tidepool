#!/usr/bin/env bash
set -euo pipefail

if [[ ${TIDEPOOL_SKIP_CODEX_SOURCE_PREFLIGHT:-} == 1 ]]; then
  exit 0
fi

repo=$(git rev-parse --show-toplevel)
codex_repo="$repo/vendor/codex"
entry=$(git -C "$repo" ls-tree HEAD -- vendor/codex)
if [[ -z $entry ]]; then
  echo 'error: HEAD has no vendor/codex gitlink; cannot verify the Nix source capture' >&2
  exit 2
fi
read -r mode type recorded path <<<"$entry"
if [[ $mode != 160000 || $type != commit || $path != vendor/codex ]]; then
  echo 'error: HEAD vendor/codex is not a recorded submodule commit' >&2
  exit 2
fi

# Codex vendors git-sourced Cargo.lock entries through cargoLock.outputHashes.
# Nix otherwise reports a missing entry only while evaluating the Codex
# derivation, often underneath a long Tidepool build trace. Catch that drift
# before entering Nix and name the file that owns the hash table.
codex_lock="$codex_repo/codex-rs/Cargo.lock"
codex_nix="$codex_repo/codex-rs/default.nix"
if [[ -f $codex_lock && -f $codex_nix ]]; then
  missing_hashes=$(awk '
    BEGIN { RS = ""; FS = "\n" }
    {
      name = version = source = ""
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^name = "/) { name = $i; sub(/^name = "/, "", name); sub(/"$/, "", name) }
        if ($i ~ /^version = "/) { version = $i; sub(/^version = "/, "", version); sub(/"$/, "", version) }
        if ($i ~ /^source = "/) { source = $i; sub(/^source = "/, "", source); sub(/"$/, "", source) }
      }
      if (name != "" && version != "" && source ~ /^git\+/) print name "-" version
    }
  ' "$codex_lock" | while IFS= read -r package; do
    if ! grep -Fq "\"$package\" =" "$codex_nix"; then
      printf '%s\n' "$package"
    fi
  done)
  if [[ -n $missing_hashes ]]; then
    echo 'error: Codex Cargo.lock has git dependencies without Nix vendor hashes:' >&2
    while IFS= read -r package; do
      [[ -z $package ]] || printf '       %s\n' "$package" >&2
    done <<<"$missing_hashes"
    echo "       Add each hash to $codex_nix in cargoLock.outputHashes." >&2
    echo '       Nix reports the expected SRI hash when that entry is set to an empty string.' >&2
    echo '       Update and push the Codex submodule commit before rebuilding Tidepool.' >&2
    exit 2
  fi
fi

if ! codex_root=$(git -C "$codex_repo" rev-parse --show-toplevel 2>/dev/null) ||
   [[ $codex_root == "$repo" ]]; then
  echo 'error: vendor/codex is not checked out, so its origin cannot be checked' >&2
  echo '       initialize the Codex submodule, then push the captured commit before entering Nix' >&2
  echo '       git -C vendor/codex push --force-with-lease origin HEAD:main' >&2
  echo '       set TIDEPOOL_SKIP_CODEX_SOURCE_PREFLIGHT=1 for offline work' >&2
  exit 2
fi
if ! advertised=$(git -C "$codex_repo" ls-remote origin); then
  echo 'error: cannot read vendor/codex origin; push the captured Codex commit before entering Nix' >&2
  echo '       git -C vendor/codex push --force-with-lease origin HEAD:main' >&2
  echo '       set TIDEPOOL_SKIP_CODEX_SOURCE_PREFLIGHT=1 for offline work' >&2
  exit 2
fi
if ! awk -v wanted="$recorded" '$1 == wanted { found = 1 } END { exit !found }' <<<"$advertised"; then
  echo "error: Nix captures vendor/codex commit $recorded, which is not advertised by vendor/codex origin" >&2
  echo '       push the captured Codex commit so the source fetch can resolve it:' >&2
  echo '       git -C vendor/codex push --force-with-lease origin HEAD:main' >&2
  echo '       set TIDEPOOL_SKIP_CODEX_SOURCE_PREFLIGHT=1 for offline work' >&2
  exit 2
fi
