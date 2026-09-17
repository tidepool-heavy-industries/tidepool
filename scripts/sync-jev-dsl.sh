#!/usr/bin/env bash
# Vendor the provider-agnostic core of ~/dev/jev-dsl (src/Jev/Core.hs and
# src/Jev/Core/*.hs) into haskell/lib/Jev/, verbatim and under the same
# module names. Refuses if the source repo has uncommitted changes to the
# files being copied — vendoring always tracks a committed revision.
set -euo pipefail

src="${1:-${SRC:-$HOME/dev/jev-dsl}}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$repo_root/haskell/lib/Jev"

if [[ ! -d "$src/.git" ]]; then
  echo "error: $src is not a git checkout" >&2
  exit 2
fi

files=(
  src/Jev/Core.hs
  src/Jev/Core/Json.hs
  src/Jev/Core/Contract.hs
  src/Jev/Core/Schema.hs
)

for f in "${files[@]}"; do
  if [[ ! -f "$src/$f" ]]; then
    echo "error: $src/$f does not exist" >&2
    exit 2
  fi
done

if ! git -C "$src" diff --quiet HEAD -- "${files[@]}"; then
  echo "error: $src has uncommitted changes to the files being vendored; commit or stash first" >&2
  exit 2
fi
if [[ -n "$(git -C "$src" status --porcelain -- "${files[@]}")" ]]; then
  echo "error: $src has untracked/uncommitted state under the files being vendored" >&2
  exit 2
fi

commit="$(git -C "$src" rev-parse HEAD)"

mkdir -p "$dest/Core"
cp "$src/src/Jev/Core.hs" "$dest/Core.hs"
cp "$src/src/Jev/Core/Json.hs" "$dest/Core/Json.hs"
cp "$src/src/Jev/Core/Contract.hs" "$dest/Core/Contract.hs"
cp "$src/src/Jev/Core/Schema.hs" "$dest/Core/Schema.hs"

{
  echo "Vendored from $src"
  echo "commit: $commit"
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "files:"
  for f in "${files[@]}"; do
    echo "  $f"
  done
} >"$dest/VENDORED"

echo "synced Jev.Core{,.Json,.Contract,.Schema} from $src@$commit into $dest"
