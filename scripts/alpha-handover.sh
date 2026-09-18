#!/usr/bin/env bash
# Run from a separate SSH shell, not a pane in the session being stopped.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
old_session="shoal-tidepool-followup"
new_session="shoal-tidepool-alpha"
cd "$repo"

command -v tmux >/dev/null
command -v just >/dev/null
test -f handoff.md

if [[ -n "${TMUX:-}" ]]; then
  echo "Run this script from a separate SSH shell outside tmux." >&2
  echo "This keeps the handover alive when the old session stops." >&2
  exit 1
fi

if tmux has-session -t "=$new_session" 2>/dev/null; then
  echo "Successor already exists; leaving both sessions untouched." >&2
  echo "Attach with: tmux attach-session -t '=$new_session'" >&2
  exit 1
fi

echo "Repository: $repo"
echo "Stopping $old_session; saved handoff: $repo/handoff.md"
if tmux has-session -t "=$old_session" 2>/dev/null; then
  tmux kill-session -t "=$old_session"
fi

# The bootstrap builds/validates matched tools and enters the pinned environment.
# Do not launch the binary directly with stale extractor/client environment.
just shoal-init -- \
  --session "$new_session" \
  --no-attach \
  --model gpt-6-astra \
  --effort medium

echo
echo "Tell the successor: Read handoff.md and continue alpha acceptance."
echo "Reattach any time: tmux attach-session -t '=$new_session'"
if [[ -t 0 && -t 1 ]]; then
  exec tmux attach-session -t "=$new_session"
fi
