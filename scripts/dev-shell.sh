#!/usr/bin/env bash
# Select the environment from committed inputs, never from mounted build trees.
set -euo pipefail

shell=default
case ${1:-} in
  --exomonad) shell=exomonad; shift ;;
  --tests) shell=tests; shift ;;
esac
source_root=$(git rev-parse --show-toplevel)
source "$source_root/scripts/toolchain-inputs.sh"
requested_cargo_target=${CARGO_TARGET_DIR:-}
env_command=()
env_assignments=()
if [[ ${1:-} == env || ${1:-} == */env ]]; then
  # Keep env's options and unrelated assignments in the executed command, but
  # resolve its target override before entering Nix or taking the fast path.
  env_command=("$1")
  shift
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -i|--ignore-environment|-)
        requested_cargo_target=
        env_command+=("$1"); shift ;;
      -u|--unset)
        if [[ $# -lt 2 ]]; then
          echo 'dev-shell: env unset option requires a variable name' >&2
          exit 2
        fi
        [[ $2 != CARGO_TARGET_DIR ]] || requested_cargo_target=
        env_command+=("$1" "$2"); shift 2 ;;
      --unset=*|-u?*)
        env_unset=${1#--unset=}
        [[ $1 == --unset=* ]] || env_unset=${1#-u}
        [[ $env_unset != CARGO_TARGET_DIR ]] || requested_cargo_target=
        env_command+=("$1"); shift ;;
      -v|--debug)
        env_command+=("$1"); shift ;;
      --)
        env_command+=("$1"); shift; break ;;
      -*)
        echo "dev-shell: unsupported env option $1 for Cargo target resolution; place env options before dev-shell" >&2
        exit 2 ;;
      *) break ;;
    esac
  done
  while [[ $# -gt 0 && $1 == *=* ]]; do
    if [[ $1 == CARGO_TARGET_DIR=* ]]; then
      requested_cargo_target=${1#CARGO_TARGET_DIR=}
    else
      env_assignments+=("$1")
    fi
    shift
  done
fi
cargo_target_args=("$source_root")
if [[ -n $requested_cargo_target ]]; then
  cargo_target_args+=("$requested_cargo_target")
fi
resolved_cargo_target=$(python3 "$source_root/scripts/cargo-target.py" "${cargo_target_args[@]}")
if [[ $requested_cargo_target != "$resolved_cargo_target" ]]; then
  echo "Cargo build directory: $resolved_cargo_target" >&2
fi
export CARGO_TARGET_DIR="$resolved_cargo_target"
if [[ ${#env_command[@]} -gt 0 ]]; then
  set -- "${env_command[@]}" "${env_assignments[@]}" "CARGO_TARGET_DIR=$resolved_cargo_target" "$@"
fi
if [[ -n ${TIDEPOOL_DEV_FLAKE:-} ]]; then
  flake=$TIDEPOOL_DEV_FLAKE
  case "$flake" in
    git+*\?*rev=*|/nix/store/*) ;;
    *) echo 'TIDEPOOL_DEV_FLAKE must name a revision-pinned Git flake or immutable store path' >&2; exit 2 ;;
  esac
else
  # Nix can open a worktree root even when Git metadata is shared. Opening the
  # .git directory directly fails under a sandbox that protects nested .git.
  if [[ $shell != exomonad ]]; then
    # Toolchain shells only need these inputs, so pin a synthetic commit over
    # them instead of HEAD: unrelated commits then reuse the same revision
    # instead of forcing Nix to re-fetch and re-evaluate the flake every time.
    tree=$(toolchain_input_tree)
    revision=$(GIT_AUTHOR_DATE='@0 +0000' GIT_COMMITTER_DATE='@0 +0000' \
      GIT_AUTHOR_NAME=dev-shell GIT_AUTHOR_EMAIL=dev-shell@invalid \
      GIT_COMMITTER_NAME=dev-shell GIT_COMMITTER_EMAIL=dev-shell@invalid \
      git commit-tree "$tree" -m "dev-shell toolchain-input pin")
    if ! git rev-parse -q --verify "refs/tidepool/dev-shell/$revision" >/dev/null; then
      git update-ref "refs/tidepool/dev-shell/$revision" "$revision"
    fi
  else
    # The Exomonad shell consumes full project inputs, so keep it pinned to the
    # checkout revision rather than a reduced toolchain-input tree.
    if ! git diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml; then
      echo 'Commit changed toolchain inputs or select TIDEPOOL_DEV_FLAKE explicitly before entering the dev shell' >&2
      exit 2
    fi
    revision=$(git rev-parse HEAD)
  fi
  flake="git+file://$source_root?rev=$revision"
fi
selection="$flake#$shell"
if [[ ${TIDEPOOL_DEV_SHELL:-} == "$selection" &&
      -n ${TIDEPOOL_DEV_GHC:-} &&
      $(command -v ghc || true) == "$TIDEPOOL_DEV_GHC" &&
      $(command -v rustc || true) == "${TIDEPOOL_DEV_RUSTC:-}" ]]; then
  exec "$@"
fi
# Retain cwd and the checkout's resolved Cargo target across the Nix boundary.
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
# Nesting one dev shell inside another (the default shell running `just`,
# whose recipe enters the exomonad shell) must not stack TMPDIRs: each level
# would add `tidepool-dev-shell.X/nix-shell.Y` to the path until a unix
# socket under it (sccache's) exceeds SUN_LEN. Place this shell's directory
# beside an enclosing dev shell's, so the depth is always one.
if [[ ${TMPDIR:-} == */tidepool-dev-shell.*/nix-shell.* ]]; then
  dev_shell_tmp_root=$(dirname "$(dirname "$TMPDIR")")
else
  dev_shell_tmp_root=${TMPDIR:-/tmp}
fi
dev_shell_tmp=$(mktemp -d -p "$dev_shell_tmp_root" tidepool-dev-shell.XXXXXX)
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
    # nix develop points TMPDIR at a directory inside $dir, so match both.
    if tr '\0' '\n' 2>/dev/null <"/proc/$pid/environ" | grep -q -e "^TMPDIR=$dir\$" -e "^TMPDIR=$dir/"; then
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
