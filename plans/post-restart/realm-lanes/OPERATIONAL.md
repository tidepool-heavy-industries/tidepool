# Operational block — copied VERBATIM into every realm-build dev spec

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
  (absolute path). NEVER `exclusive` mode.
- `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
  test shard (persistent per-worktree, not mktemp).
- Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
  never fable.
- Never path-unscoped `pkill -f`; scope kills to PID or full worktree
  path.
- Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
  protected. Grep/Read over LSP; no per-worktree rust-analyzer.
- STANDING DEV-SPEC RULE (from the generic-surface wave, 2026-08-08 — copy
  into every dev spec, both sub-TLs): a "pre-existing/inherited red" claim
  requires a cache-consistent A/B baseline run in the dev's OWN worktree —
  same compile-cache state on both legs, the dev's diff absent vs present.
  An argument from "my diff doesn't touch the failing test files" is
  invalid for global surfaces (prelude exports, pragma/extension sets,
  shared flags): every Haskell compile is downstream of those whether or
  not its file is in the diff. Note the cache confound explicitly: a
  fingerprint-invalidating change makes a naive comparison measure
  cold-vs-warm, not the diff. Empirically (this wave): both devs given
  this instruction produced sound baselines; the one that wasn't produced
  a plausible wrong argument.
