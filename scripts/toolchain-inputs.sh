# Shared committed input boundary for the default dev shell and native launcher.
toolchain_input_tree() {
  local git_bin=${1:-git} revision=${2:-HEAD}
  if ! "$git_bin" diff --quiet HEAD -- flake.nix flake.lock rust-toolchain.toml nix; then
    echo 'Commit changed toolchain inputs before native configuration or execution' >&2
    return 2
  fi
  "$git_bin" ls-tree "$revision" -- flake.nix flake.lock rust-toolchain.toml nix | "$git_bin" mktree
}
