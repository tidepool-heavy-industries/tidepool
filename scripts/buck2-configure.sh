#!/usr/bin/env bash
set -euo pipefail

# Prepare and retain the selected pinned Nix outputs before publishing config.
cd "$(dirname "$0")/.."

usage() {
  printf 'Usage: scripts/buck2-configure.sh [--tests]\n'
}

test_toolchain=false
for argument in "$@"; do
  case "$argument" in
    --tests) test_toolchain=true ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

if ! mountpoint -q "$PWD/buck-out"; then
  printf 'Provision a real per-checkout buck-out bind mount before configuring Buck: %s/buck-out\n' "$PWD" >&2
  exit 1
fi

remote_enabled=false
remote_toolchain=""
if [ "${TIDEPOOL_BUCK_REMOTE:-false}" = true ]; then
  platform_file=${TIDEPOOL_BUCK_PLATFORM_FILE:-/etc/swarm-build/platform}
  test -r "$platform_file" || { echo "Missing remote platform identity: $platform_file" >&2; exit 1; }
  remote_toolchain="$(cat "$platform_file")"
  [[ $remote_toolchain =~ ^[0-9a-f]{64}$ ]] || { echo 'Invalid remote platform identity' >&2; exit 1; }
  remote_address=${TIDEPOOL_BUCK_REMOTE_ADDRESS:-grpc://127.0.0.1:50051}
  [[ $remote_address =~ ^grpc://(localhost|127\.0\.0\.1):[0-9]+$ ]] || { echo 'Remote endpoint must use an SSH tunnel on loopback' >&2; exit 1; }
  remote_enabled=true
fi

if [[ -z ${TIDEPOOL_DEV_SHELL:-} ]]; then
  exec bash scripts/dev-shell.sh bash scripts/buck2-configure.sh "$@"
fi
flake_source=${TIDEPOOL_DEV_SHELL%#*}
case "$flake_source" in
  git+*\?*rev=*|/nix/store/*) ;;
  *) echo 'Configuration requires a revision-pinned Git flake or immutable store path' >&2; exit 2 ;;
esac
# An inherited default shell can outlive a committed pin change. Its selection
# must still represent the checkout inputs unless an immutable override was
# explicitly selected through the dev-shell owner.
selection_mode=explicit
toolchain_tree=
local_flake_prefix="git+file://$PWD?rev="
if [[ -z ${TIDEPOOL_DEV_FLAKE:-} && $flake_source == "$local_flake_prefix"* ]]; then
  selection_mode=checkout
  source scripts/toolchain-inputs.sh
  toolchain_tree=$(toolchain_input_tree)
  selected_revision=${flake_source#"$local_flake_prefix"}
  selected_revision=${selected_revision%%&*}
  selected_tree=$(toolchain_input_tree git "$selected_revision")
  [[ $selected_tree == "$toolchain_tree" ]] || {
    echo 'The inherited dev shell has stale toolchain inputs; enter scripts/dev-shell.sh before configuration' >&2
    exit 2
  }
fi
system="$(nix eval --raw --impure --expr 'builtins.currentSystem')"
# Each invocation owns durable indirect GC roots. Failed and previous generations
# remain available for inspection; checkout preparation cannot determine whether
# a frozen bundle or live process still needs an older generation.
mkdir -p "$PWD/.buck2-toolchains/generations"
generation=$(mktemp -d "$PWD/.buck2-toolchains/generations/generation.XXXXXXXX")
mkdir "$generation/roots"
: > "$generation/outputs.tsv"
printf 'checkout=%s\nuid=%s\nselection=%s\nselection_mode=%s\ntests=%s\ntoolchain_tree=%s\n' \
  "$PWD" "$(id -u)" "$TIDEPOOL_DEV_SHELL" "$selection_mode" "$test_toolchain" "$toolchain_tree" > "$generation/owner"
printf 'preparing\n' > "$generation/status"
tmp_config=
finish_preparation() {
  local result=$?
  [[ -z $tmp_config ]] || rm -f -- "$tmp_config"
  if [[ $result != 0 ]]; then
    printf 'failed (exit %s)\n' "$result" > "$generation/status"
    printf 'Preparation failed; retained generation: %s\n' "$generation" >&2
  fi
}
trap finish_preparation EXIT
printf 'Preparing toolchain generation: %s\n' "$generation" >&2
selected_output() {
  local name=$1 reference=$2 output root
  output=$(nix eval --raw "$reference.outPath") || return
  # TSV is an inspectable record of this exact selection, not a second roster.
  [[ $reference != *$'\t'* && $reference != *$'\n'* &&
     $output != *$'\t'* && $output != *$'\n'* ]] || {
    echo 'Invalid selected Nix output record' >&2; return 1;
  }
  root=$generation/roots/$name
  printf '%s\t%s\t%s\t%s\n' "$name" "$reference" "$output" "$root" >> "$generation/outputs.tsv"
  # --out-link registers an indirect GC root as well as realizing the output.
  nix build --no-write-lock-file --out-link "$root" "$reference" >&2 || return
  [[ -L $root && $(readlink -f -- "$root") == "$output" && -d $output ]] || {
    printf 'Selected output/root mismatch: %s -> %s\n' "$root" "$output" >&2; return 1;
  }
  nix path-info -- "$output" >/dev/null || return
  printf '%s\n' "$output"
}
output_path() {
  selected_output "buck-$1" "${flake_source}#packages.${system}.buck-$1"
}
buck2="$(output_path buck2)"
[[ -x $buck2/bin/buck2 ]] || { echo 'Prepared Buck executable is unavailable' >&2; exit 1; }
rust="$(output_path rust)"
ghc="$(output_path ghc)"
ghc_libdir="$("${ghc}/bin/ghc" --print-libdir)"
# Test packages select a distinct wrapper/package environment. Never substitute
# this libdir or compiler for the production worker's declared GHC closure.
test_ghc=
test_ghc_bin=
test_ghc_pkg=
test_haddock=
test_ghc_libdir=
haskell_test_closure=
jev_sources=
if [[ $test_toolchain == true ]]; then
  test_ghc_root="$(output_path test-ghc)"
  test_ghc=$test_ghc_root/bin/ghc
  test_ghc_bin=$test_ghc_root/bin
  test_ghc_pkg=$test_ghc_root/bin/ghc-pkg
  test_haddock=$test_ghc_root/bin/haddock
  test_ghc_libdir="$("$test_ghc" --print-libdir)"
  haskell_test_closure="$(output_path haskell-test-closure)"
  jev_sources="$(output_path jev-sources)"
fi
cc="$(output_path cc)"
lld="$(output_path lld)"
[[ -x $lld/bin/ld.lld ]] || { echo 'Prepared pinned LLD executable is unavailable' >&2; exit 1; }
binutils="$(output_path binutils)"
node="$(output_path node)"
npm_cache="$(output_path npm-cache)"
browser_node="$(output_path browser-node)"
browser_npm_cache="$(output_path browser-npm-cache)"
playwright_browsers="$(output_path playwright-browsers)"
browser_test_closure="$(output_path browser-test-closure)"
test_tools="$(output_path test-tools)"
test_tools_closure="$(output_path test-tools-closure)"
matched_harness_source="$(output_path matched-harness-source)"
git_path="$(output_path test-git)"
bash_path="$(output_path bash)"
coreutils="$(output_path coreutils)"
tar_path="$(output_path tar)"
gzip="$(output_path gzip)"
python="$(output_path python)"
bubblewrap="$(output_path bubblewrap)"
workspace_revision=$("$python/bin/python3" scripts/workspace-git-resource.py \
  --source-root "$PWD" --output "$generation/workspace-git-resource" --git "$git_path/bin/git")
workspace_git_resource=$(nix store add-path --name "exomonad-workspace-$workspace_revision" "$generation/workspace-git-resource")
nix-store --add-root "$generation/roots/workspace-git-resource" --indirect --realise "$workspace_git_resource" >/dev/null
printf 'workspace-git-resource\tgitlink:%s\t%s\t%s\n' \
  "$workspace_revision" \
  "$workspace_git_resource" "$generation/roots/workspace-git-resource" >> "$generation/outputs.tsv"
# Runtime commands are pinned tool inputs, independently of native project products.
exomonad_runtime_tools="$(output_path exomonad-runtime-tools)"
cmake="$(output_path cmake)"
perl="$(output_path perl)"
pkg_config="$(output_path pkg-config)"

tmp_config=$(mktemp "$PWD/.buckconfig.local.XXXXXX")
cat > "$tmp_config" <<EOF
# Generated by scripts/buck2-configure.sh from flake.lock.
# Retained toolchain generation: $generation
[nix]
buck2 = $buck2/bin/buck2
rustc = $rust/bin/rustc
rustdoc = $rust/bin/rustdoc
rustfmt = $rust/bin/rustfmt
clippy = $rust/bin/clippy-driver
ghc = $ghc/bin/ghc
ghc_bin = $ghc/bin
ghc_pkg = $ghc/bin/ghc-pkg
haddock = $ghc/bin/haddock
ghc_libdir = $ghc_libdir
test_ghc = $test_ghc
test_ghc_bin = $test_ghc_bin
test_ghc_pkg = $test_ghc_pkg
test_haddock = $test_haddock
test_ghc_libdir = $test_ghc_libdir
haskell_test_closure = $haskell_test_closure
jev_sources = $jev_sources
cc = $cc/bin/cc
cxx = $cc/bin/c++
lld_bin = $lld/bin
ar = $binutils/bin/ar
node = $node/bin/node
npm_cache = $npm_cache
npm = $node/bin/npm
browser_node = $browser_node/bin/node
browser_npm = $browser_node/bin/npm
browser_npm_cache = $browser_npm_cache
playwright_browsers = $playwright_browsers
browser_test_closure = $browser_test_closure
test_tools = $test_tools
test_tools_closure = $test_tools_closure
matched_harness_source = $matched_harness_source
workspace_git_resource = $workspace_git_resource
git = $git_path/bin/git
bash = $bash_path/bin/bash
coreutils = $coreutils/bin
sleep = $coreutils/bin/sleep
tar = $tar_path/bin/tar
gzip = $gzip/bin/gzip
python = $python/bin/python3
bubblewrap = $bubblewrap/bin/bwrap
exomonad_runtime_tools = $exomonad_runtime_tools
cmake = $cmake/bin/cmake
perl = $perl/bin/perl
pkg_config = $pkg_config/bin/pkg-config
action_path = $rust/bin:$cc/bin:$lld/bin:$binutils/bin:$cmake/bin:$perl/bin:$pkg_config/bin:$python/bin:$bash_path/bin:$coreutils/bin:$tar_path/bin:$gzip/bin:$node/bin:$git_path/bin

[remote]
enabled = $remote_enabled
toolchain = $remote_toolchain
EOF
if [ "$remote_enabled" = true ]; then
  cat >> "$tmp_config" <<EOF

[buck2_re_client]
action_cache_address = $remote_address
engine_address = $remote_address
cas_address = $remote_address
tls = false
instance_name = swarm
EOF
fi
# Validate the entire retained generation again before switching configuration.
while IFS=$'\t' read -r name reference output root; do
  [[ -L $root && $(readlink -f -- "$root") == "$output" && -d $output ]] || {
    printf 'Prepared output is unavailable: %s\n' "$name" >&2; exit 1;
  }
  nix path-info -- "$output" >/dev/null
done < "$generation/outputs.tsv"
[[ -d $ghc_libdir && ( $test_toolchain != true || -d $test_ghc_libdir ) ]] || {
  echo 'Prepared GHC libdir is unavailable' >&2; exit 1;
}
chmod 0644 "$tmp_config"
cp -- "$tmp_config" "$generation/config"
mv -f -- "$tmp_config" .buckconfig.local
tmp_config=
printf 'configured\n' > "$generation/status"
trap - EXIT
printf 'Wrote %s/.buckconfig.local\nRetained toolchain generation: %s\n' "$PWD" "$generation"
