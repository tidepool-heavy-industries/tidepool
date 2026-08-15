#!/usr/bin/env bash
# Fast-forward the companion memory store's checked-out branch to the newest
# curator worktree branch — OPERATOR tooling, deliberately outside the Rust
# runtime: PRD 19 frozen rule, "no git workflow verbs in tidepool-worktree";
# the runtime observes repositories, humans and agents do the git work.
#
#   scripts/companion-memory-sync.sh [store]   # default ~/.local/share/tidepool/companion-memory
#
# STRICTLY ff-only: if master diverged (you committed to it directly), this
# fails loudly instead of merging — that divergence deserves your eyes.
set -euo pipefail
STORE="${1:-$HOME/.local/share/tidepool/companion-memory}"
BRANCH=$(git -C "$STORE" for-each-ref --sort=-committerdate --count=1 \
  --format='%(refname:short)' 'refs/heads/tidepool/worktree/memory-curator-*')
[ -n "$BRANCH" ] || { echo "no curator branch found in $STORE" >&2; exit 1; }
git -C "$STORE" merge --ff-only "$BRANCH"
git -C "$STORE" log --oneline -3
