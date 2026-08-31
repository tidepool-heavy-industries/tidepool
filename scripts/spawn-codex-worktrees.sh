#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/spawn-codex-worktrees.sh [options] WORKTREE...

Open one tmux window with one Codex process per worktree, arranged in tiled
panes. A WORKTREE may be a path or a name under .exo/worktrees. Every worktree
must contain prompt.md.tmp. Every worker reads the repository's shared
scripts/codex-worktree-guidance.md before its parcel prompt.

Options:
  --session NAME   tmux session to create/use (default: current session when
                   inside tmux, otherwise tidepool-codex)
  --window NAME    new tmux window name (default: codex-<timestamp>)
  --sol            use gpt-5.6-sol (default)
  --terra          use gpt-5.6-terra for clearly bounded mechanical work
  --model MODEL    use an explicit Codex model
  --effort LEVEL   model reasoning effort (defaults: Sol low, Terra medium)
  --attach          attach after creating the window when outside tmux
  --dry-run         validate and print the pane plan without changing tmux
  -h, --help        show this help

Examples:
  scripts/spawn-codex-worktrees.sh \
    actor-test-hygiene repl-name-plane-atomicity

  scripts/spawn-codex-worktrees.sh --terra --effort medium \
    actor-test-hygiene

  scripts/spawn-codex-worktrees.sh --session overnight --attach \
    .exo/worktrees/compiler-endpoint-identity
EOF
}

die() {
  echo "spawn-codex-worktrees: $*" >&2
  exit 2
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -- "$script_dir/.." && pwd -P)

session=""
window="codex-$(date +%Y%m%d-%H%M%S)"
model="gpt-5.6-sol"
effort="low"
effort_explicit=false
attach=false
dry_run=false
declare -a requested=()

while (($#)); do
  case "$1" in
    --session)
      (($# >= 2)) || die "--session requires a value"
      session=$2
      shift 2
      ;;
    --window)
      (($# >= 2)) || die "--window requires a value"
      window=$2
      shift 2
      ;;
    --model)
      (($# >= 2)) || die "--model requires a value"
      model=$2
      shift 2
      ;;
    --sol)
      model="gpt-5.6-sol"
      if ! $effort_explicit; then
        effort="low"
      fi
      shift
      ;;
    --terra)
      model="gpt-5.6-terra"
      if ! $effort_explicit; then
        effort="medium"
      fi
      shift
      ;;
    --effort)
      (($# >= 2)) || die "--effort requires a value"
      effort=$2
      effort_explicit=true
      shift 2
      ;;
    --attach)
      attach=true
      shift
      ;;
    --dry-run)
      dry_run=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      requested+=("$@")
      break
      ;;
    -*)
      die "unknown option: $1"
      ;;
    *)
      requested+=("$1")
      shift
      ;;
  esac
done

((${#requested[@]} > 0)) || die "provide at least one worktree"
[[ -n $window ]] || die "window name must not be empty"
[[ -n $model ]] || die "model must not be empty"
[[ -n $effort ]] || die "reasoning effort must not be empty"

declare -a worktrees=()
declare -A seen=()
for item in "${requested[@]}"; do
  candidate=$item
  if [[ ! -d $candidate ]]; then
    candidate="$repo_root/.exo/worktrees/$item"
  fi
  [[ -d $candidate ]] || die "worktree does not exist: $item"

  worktree=$(cd -- "$candidate" && pwd -P)
  git -C "$worktree" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || die "not a Git worktree: $worktree"
  [[ -f $worktree/prompt.md.tmp ]] \
    || die "missing prompt.md.tmp: $worktree"
  [[ -z ${seen[$worktree]+present} ]] \
    || die "worktree listed more than once: $worktree"
  seen[$worktree]=1
  worktrees+=("$worktree")
done

guidance="$repo_root/scripts/codex-worktree-guidance.md"
[[ -f $guidance ]] || die "missing shared worker guidance: $guidance"
worker_prompt=$(printf 'read %s and prompt.md.tmp, then begin work; both are authoritative, with the narrower parcel prompt winning on conflicts' "$guidance")
pane_command=$(printf 'exec codex --strict-config -m %q -c %q %q' \
  "$model" "model_reasoning_effort=$effort" "$worker_prompt")

if $dry_run; then
  printf 'session: %s\n' "${session:-<current-or-tidepool-codex>}"
  printf 'window:  %s\n' "$window"
  for worktree in "${worktrees[@]}"; do
    printf 'pane:    %q\n' "$worktree"
    printf 'command: %s\n' "$pane_command"
  done
  exit 0
fi

command -v tmux >/dev/null 2>&1 || die "tmux is not installed or not on PATH"
command -v codex >/dev/null 2>&1 || die "codex is not installed or not on PATH"

inside_tmux=false
if [[ -n ${TMUX:-} ]]; then
  inside_tmux=true
  if [[ -z $session ]]; then
    session=$(tmux display-message -p '#S')
  fi
fi
[[ -n $session ]] || session=tidepool-codex

first=${worktrees[0]}
if tmux has-session -t "$session" 2>/dev/null; then
  window_id=$(tmux new-window -d -P -F '#{window_id}' \
    -t "$session:" -n "$window" -c "$first" "$pane_command")
else
  window_id=$(tmux new-session -d -P -F '#{window_id}' \
    -s "$session" -n "$window" -c "$first" "$pane_command")
fi

first_pane=$(tmux display-message -p -t "$window_id" '#{pane_id}')
tmux select-pane -t "$first_pane" -T "$(basename -- "$first")"

for worktree in "${worktrees[@]:1}"; do
  pane_id=$(tmux split-window -d -P -F '#{pane_id}' \
    -t "$window_id" -c "$worktree" "$pane_command")
  tmux select-pane -t "$pane_id" -T "$(basename -- "$worktree")"
  tmux select-layout -t "$window_id" tiled >/dev/null
done

tmux select-layout -t "$window_id" tiled >/dev/null

if $inside_tmux; then
  tmux select-window -t "$window_id"
  printf 'started %d Codex panes in %s:%s\n' \
    "${#worktrees[@]}" "$session" "$window"
elif $attach; then
  tmux select-window -t "$window_id"
  exec tmux attach-session -t "$session"
else
  printf 'started %d Codex panes in %s:%s\n' \
    "${#worktrees[@]}" "$session" "$window"
  printf 'attach with: tmux attach-session -t %q\n' "$session"
fi
