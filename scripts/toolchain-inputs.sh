# Shared committed input boundary for the default dev shell and native launcher.
toolchain_input_tree() (
  set -o pipefail
  local git_bin=${1:-git} revision=${2:-HEAD} snapshot_index untracked_inputs
  # Local sources consumed by the default shell and native toolchain outputs.
  # Full-source flake checks retain their separate checkout-revision selection.
  local inputs=(flake.nix flake.lock rust-toolchain.toml nix bridge/haskell/resume)
  untracked_inputs=$("$git_bin" ls-files --others --exclude-standard -- "${inputs[@]}") || return
  if ! "$git_bin" diff --quiet HEAD -- "${inputs[@]}" || [[ -n $untracked_inputs ]]; then
    echo 'Commit changed toolchain inputs before native configuration or execution' >&2
    return 2
  fi
  # An isolated index preserves nested paths without adding unrelated siblings
  # to the synthetic tree or changing the checkout's index.
  snapshot_index=$(mktemp) || return
  trap 'rm -f -- "$snapshot_index" "$snapshot_index.lock"' EXIT
  GIT_INDEX_FILE=$snapshot_index "$git_bin" read-tree --empty || return
  "$git_bin" ls-tree -rz "$revision" -- "${inputs[@]}" |
    GIT_INDEX_FILE=$snapshot_index "$git_bin" update-index -z --index-info || return
  GIT_INDEX_FILE=$snapshot_index "$git_bin" write-tree
)
