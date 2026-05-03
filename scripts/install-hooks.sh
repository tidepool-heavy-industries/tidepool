#!/usr/bin/env bash
# Install the tracked git hooks into .git/hooks via symlink.
# Re-run safely; existing symlinks are replaced.
set -e

repo_root="$(git rev-parse --show-toplevel)"
src_dir="$repo_root/scripts/git-hooks"
dst_dir="$repo_root/.git/hooks"

if [[ ! -d "$src_dir" ]]; then
  echo "no $src_dir directory; nothing to install" >&2
  exit 1
fi

mkdir -p "$dst_dir"

for hook_path in "$src_dir"/*; do
  [[ -f "$hook_path" ]] || continue
  hook_name="$(basename "$hook_path")"
  dst="$dst_dir/$hook_name"

  if [[ -L "$dst" || -f "$dst" ]]; then
    rm -f "$dst"
  fi

  ln -s "../../scripts/git-hooks/$hook_name" "$dst"
  chmod +x "$hook_path"
  echo "installed $hook_name -> $dst"
done

echo ""
echo "Hooks installed. Bypass any single push with: TIDEPOOL_SKIP_PRE_PUSH=1 git push"
