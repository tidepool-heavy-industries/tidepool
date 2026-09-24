#!/usr/bin/env bash
# Select the environment from committed inputs, never from mounted build trees.
set -euo pipefail

shell=default
if [[ ${1:-} == --exomonad ]]; then
  shell=exomonad
  shift
fi
if [[ $shell == exomonad ]]; then
  # Only exomonad's flake forces the codex path input, so only it needs the
  # vendor/codex source capture verified before Nix evaluates anything.
  bash scripts/codex-source-preflight.sh
fi

if [[ -n ${TIDEPOOL_DEV_FLAKE:-} ]]; then
  flake=$TIDEPOOL_DEV_FLAKE
  case "$flake" in
    git+*\?*rev=*|/nix/store/*) ;;
    *) echo 'TIDEPOOL_DEV_FLAKE must name a revision-pinned Git flake or immutable store path' >&2; exit 2 ;;
  esac
else
  common=$(git rev-parse --path-format=absolute --git-common-dir)
  if [[ $shell == default ]]; then
    # default only needs the toolchain inputs, so pin a synthetic commit over
    # them instead of HEAD: unrelated commits then reuse the same revision
    # instead of forcing Nix to re-fetch and re-evaluate the flake every time.
    if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml nix; then
      echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
      exit 2
    fi
    tree=$(git ls-tree HEAD -- flake.nix flake.lock rust-toolchain.toml nix | git mktree)
    revision=$(GIT_AUTHOR_DATE='@0 +0000' GIT_COMMITTER_DATE='@0 +0000' \
      GIT_AUTHOR_NAME=dev-shell GIT_AUTHOR_EMAIL=dev-shell@invalid \
      GIT_COMMITTER_NAME=dev-shell GIT_COMMITTER_EMAIL=dev-shell@invalid \
      git commit-tree "$tree" -m "dev-shell toolchain-input pin")
    if ! git rev-parse -q --verify "refs/tidepool/dev-shell/$revision" >/dev/null; then
      git update-ref "refs/tidepool/dev-shell/$revision" "$revision"
    fi
  else
    # exomonad forces the codex path input, which resolves against the real
    # worktree, so it stays pinned to HEAD rather than the reduced tree.
    if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml; then
      echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
      exit 2
    fi
    revision=$(git rev-parse HEAD)
  fi
  flake="git+file://$common?rev=$revision"
fi
selection="$flake#$shell"
if [[ ${TIDEPOOL_DEV_SHELL:-} == "$selection" &&
      -n ${TIDEPOOL_DEV_GHC:-} &&
      $(command -v ghc || true) == "$TIDEPOOL_DEV_GHC" &&
      $(command -v rustc || true) == "${TIDEPOOL_DEV_RUSTC:-}" ]]; then
  exec "$@"
fi
# Retain cwd and CARGO_TARGET_DIR: only the environment comes from the flake.
#
# `nix develop --command CMD` builds its own NIX_BUILD_TOP with
# `mktemp -d -t nix-shell.XXXXXX` inside a generated rc script, then execve's
# CMD in place of that script's shell. The execve discards the process before
# any trap the rc script set can remove NIX_BUILD_TOP, so every invocation
# leaks a /tmp/nix-shell.XXXXXX directory (and its nix-develop-*/
# nix-git-submodules.* siblings); this happens inside nix itself, not in
# anything this script execs, so it cannot be fixed by avoiding exec here.
# Give nix's mktemp a private TMPDIR we control instead, and remove that
# whole directory ourselves once the command exits, keeping its exit status
# (nix propagates signal-terminated commands as the usual 128+signal status).
dev_shell_tmp=$(mktemp -d -t tidepool-dev-shell.XXXXXX)
cleanup_dev_shell_tmp() {
  local dir=$1 pid
  # A command run in this shell can start a detached process that outlives
  # this script (e.g. `just daemon-start`'s setsid'd persistent compile
  # daemon keeper) and inherits this TMPDIR. Removing the directory under a
  # still-running process would break it, so only remove it when no live
  # process still has it as $TMPDIR; otherwise leave it for a later
  # invocation to collect once nothing references it any more.
  for pid in /proc/[0-9]*; do
    pid=${pid#/proc/}
    [[ $pid == "$$" ]] && continue
    [[ -r /proc/$pid/environ ]] || continue
    if tr '\0' '\n' 2>/dev/null <"/proc/$pid/environ" | grep -qxF "TMPDIR=$dir"; then
      return 0
    fi
  done
  rm -rf "$dir"
}
trap 'cleanup_dev_shell_tmp "$dev_shell_tmp"' EXIT
TMPDIR="$dev_shell_tmp" nix develop "$selection" --command bash -c '
  export TIDEPOOL_DEV_SHELL="$1"
  export TIDEPOOL_DEV_GHC="$(command -v ghc)"
  export TIDEPOOL_DEV_RUSTC="$(command -v rustc)"
  shift
  exec "$@"
' dev-shell "$selection" "$@"
