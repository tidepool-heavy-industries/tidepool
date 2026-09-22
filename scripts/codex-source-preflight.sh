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
