#!/usr/bin/env bash
# Start the alpha smoke run: one Sol root agent in this repository's own
# workspace, given plans/jev-lab/alpha-smoke-prompt.md.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

session=${EXOMONAD_SMOKE_SESSION:-exomonad-alpha-smoke}
brief=plans/jev-lab/alpha-smoke-prompt.md

if [[ -z "${TYPESAFE_API_KEY:-}" ]]; then
  key_file="$HOME/.config/typesafe/api-key"
  if [[ ! -r "$key_file" ]]; then
    echo "set TYPESAFE_API_KEY, or put the key in $key_file: the run is about Jev" >&2
    exit 2
  fi
  TYPESAFE_API_KEY="$(< "$key_file")"
  export TYPESAFE_API_KEY
fi

exomonad/scripts/exomonad-init.sh --session "$session" --model gpt-6-sol --effort medium --no-attach "$@"

echo "==> waiting for the root agent's window"
# The root's window is named for its actor, `exomonad-root [<id>@<incarnation>]`.
root_window() {
  tmux list-windows -t "$session" -F '#{window_index} #{window_name}' 2>/dev/null |
    awk '$2 == "exomonad-root" { print $1; exit }'
}
for _ in $(seq 1 120); do
  [[ -n "$(root_window)" ]] && break
  sleep 2
done
window=$(root_window)
if [[ -z "$window" ]]; then
  echo "the root agent's window never appeared in tmux session $session; the brief was not sent" >&2
  exit 1
fi
# The client needs a moment after its window exists before it reads input.
sleep 10

tmux load-buffer -b exomonad-smoke-brief "$brief"
tmux paste-buffer -p -d -b exomonad-smoke-brief -t "$session:$window"
sleep 1
tmux send-keys -t "$session:$window" Enter

echo "==> brief sent. Attach with: tmux attach -t $session"
