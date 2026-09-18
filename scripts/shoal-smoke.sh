#!/usr/bin/env bash
# Start the alpha smoke run: one Sol root agent in this repository's own
# workspace, given plans/jev-lab/alpha-smoke-prompt.md.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

session=${SHOAL_SMOKE_SESSION:-shoal-alpha-smoke}
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

scripts/shoal-init.sh --session "$session" --model gpt-5.6-sol --effort medium --no-attach "$@"

echo "==> waiting for the root agent's window"
for _ in $(seq 1 120); do
  if tmux list-windows -t "$session" -F '#{window_name}' 2>/dev/null | grep -qx Root; then
    break
  fi
  sleep 2
done
if ! tmux list-windows -t "$session" -F '#{window_name}' | grep -qx Root; then
  echo "the Root window never appeared in tmux session $session; the brief was not sent" >&2
  exit 1
fi
# The client needs a moment after its window exists before it reads input.
sleep 10

tmux load-buffer -b shoal-smoke-brief "$brief"
tmux paste-buffer -p -d -b shoal-smoke-brief -t "$session:Root"
sleep 1
tmux send-keys -t "$session:Root" Enter

echo "==> brief sent. Attach with: tmux attach -t $session"
