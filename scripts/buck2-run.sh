#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mountpoint -q "$PWD/buck-out" || { echo 'Buck requires the per-checkout buck-out bind mount' >&2; exit 1; }
[[ -f .buckconfig.local ]] || { echo 'Run scripts/buck2-configure.sh first' >&2; exit 1; }

refuse() {
  printf 'Configured Buck toolchain is unavailable or stale: %s. Run scripts/buck2-configure.sh.\n' "$1" >&2
  exit 1
}
# The published copy and its retained output records jointly own this launch.
# No Nix evaluation or shell marker supplies an alternative executable.
record_value() {
  local prefix=$1 path=$2 line
  while IFS= read -r line || [[ -n $line ]]; do
    [[ $line != "$prefix"* ]] || printf '%s\n' "${line#"$prefix"}"
  done < "$path"
}
same_file_bytes() {
  local left_fd right_fd left right left_status right_status result=0
  exec {left_fd}< "$1" || return 1
  exec {right_fd}< "$2" || { exec {left_fd}<&-; return 1; }
  # NUL-delimited reads retain newlines; matching read status also compares
  # each NUL delimiter and the final EOF without storing NUL in a Bash string.
  while :; do
    left_status=0 right_status=0
    IFS= read -r -d '' left <&"$left_fd" || left_status=$?
    IFS= read -r -d '' right <&"$right_fd" || right_status=$?
    if [[ $left != "$right" || $left_status != "$right_status" ]]; then
      result=1
      break
    fi
    [[ $left_status == 0 ]] || break
  done
  exec {left_fd}<&-
  exec {right_fd}<&-
  return "$result"
}
generation=$(record_value '# Retained toolchain generation: ' .buckconfig.local)
[[ $generation == "$PWD/.buck2-toolchains/generations/"generation.* &&
   -d $generation && -f $generation/status && -f $generation/config &&
   -f $generation/owner && -s $generation/outputs.tsv ]] || refuse 'missing retained generation'
[[ $(cat "$generation/status") == configured ]] || refuse 'generation was not configured'
same_file_bytes .buckconfig.local "$generation/config" || refuse 'published configuration changed'
owner_value() { record_value "$1=" "$generation/owner"; }
[[ $(owner_value checkout) == "$PWD" && $(owner_value uid) == "$(id -u)" ]] || refuse 'generation belongs to another checkout or user'
selection=$(owner_value selection)
flake_source=${selection%#*}
case "$flake_source" in
  git+*\?*rev=*|/nix/store/*) ;;
  *) refuse 'unpinned flake selection' ;;
esac
config_value() { record_value "$1 = " .buckconfig.local; }
buck_bin=$(config_value buck2)
action_path=$(config_value action_path)
[[ -n $buck_bin && -n $action_path ]] || refuse 'missing configured Buck executable or action PATH'
outputs=()
buck_output=
git_output=
workspace_gitlink_revision=
while IFS=$'\t' read -r name reference output root; do
  [[ $root == "$generation/roots/$name" && -L $root &&
     $(readlink -f -- "$root") == "$output" && -d $output ]] || refuse "retained output $name"
  case "$name" in
    buck-*) [[ $reference == "$flake_source#"* ]] || refuse "output selection $name" ;;
  esac
  outputs+=("$output")
  [[ $name != buck-buck2 ]] || buck_output=$output
  [[ $name != buck-test-git ]] || git_output=$output
  if [[ $name == workspace-git-resource ]]; then
    [[ -z $workspace_gitlink_revision && $reference == gitlink:* ]] || refuse 'workspace Gitlink resource record'
    workspace_gitlink_revision=${reference#gitlink:}
    [[ $workspace_gitlink_revision =~ ^[0-9a-f]{40}$ ]] || refuse 'workspace Gitlink resource revision'
  fi
done < "$generation/outputs.tsv"
[[ -n $buck_output && $buck_bin == "$buck_output/bin/buck2" && -x $buck_bin ]] || refuse 'Buck executable does not match its retained output'
[[ $action_path != :* && $action_path != *: && $action_path != *::* ]] || refuse 'empty action PATH entry'
IFS=: read -r -a action_dirs <<< "$action_path"
for directory in "${action_dirs[@]}"; do
  found=false
  for output in "${outputs[@]}"; do
    [[ $directory != "$output/bin" ]] || found=true
  done
  [[ $found == true && -d $directory ]] || refuse 'action PATH is not retained'
done
# Toolchain changes require preparation; unrelated source commits reuse this
# exact generation. Use the retained Git executable for the source comparison.
export PATH="$action_path:/run/current-system/sw/bin"
git_bin=$(config_value git)
[[ -n $git_output && $git_bin == "$git_output/bin/git" && -x $git_bin ]] || refuse 'configured Git executable is unavailable'
case $(owner_value selection_mode) in
  checkout)
    source scripts/toolchain-inputs.sh
    current_tree=$(toolchain_input_tree "$git_bin") || refuse 'changed toolchain inputs'
    [[ $(owner_value toolchain_tree) == "$current_tree" ]] || refuse 'changed toolchain input pin'
    ;;
  explicit) ;;
  *) refuse 'missing generation selection mode' ;;
esac
[[ -n $workspace_gitlink_revision ]] || refuse 'missing retained workspace Gitlink resource'
workspace_gitlink_line=$("$git_bin" -C "$PWD" ls-tree HEAD -- .exomonad/workspace) || refuse 'cannot read committed workspace Gitlink'
IFS=$'\t' read -r workspace_gitlink_metadata workspace_gitlink_path <<< "$workspace_gitlink_line"
read -r workspace_gitlink_mode workspace_gitlink_kind current_workspace_revision <<< "$workspace_gitlink_metadata"
if [[ $workspace_gitlink_mode != 160000 || $workspace_gitlink_kind != commit ||
      ! $current_workspace_revision =~ ^[0-9a-f]{40}$ ||
      $workspace_gitlink_path != .exomonad/workspace ]]; then
  refuse 'missing committed workspace Gitlink'
fi
[[ $workspace_gitlink_revision == "$current_workspace_revision" ]] || refuse 'changed workspace Gitlink'
if [[ -n ${TIDEPOOL_BUCK_CONFIG_FILE:-} ]]; then
  shared_config=$(mktemp)
  trap 'rm -f -- "$shared_config"' EXIT
  if ! python3 scripts/buck2-config-args.py "$TIDEPOOL_BUCK_CONFIG_FILE" -- "$@" > "$shared_config"; then
    refuse 'shared Buck configuration is invalid or conflicts with command-line configuration'
  fi
  mapfile -d '' -t configured_args < "$shared_config"
  rm -f -- "$shared_config"
  trap - EXIT
  exec "$buck_bin" "${configured_args[@]}"
fi
exec "$buck_bin" "$@"
