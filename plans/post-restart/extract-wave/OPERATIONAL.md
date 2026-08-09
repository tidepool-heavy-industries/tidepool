# extract-wave operational block

**Copy this file's "Block" section VERBATIM into EVERY dev spec you write.**
It is not a summary — it is the literal text devs must receive.

## Namespace allocation (extract-wave, allocated by the wave TL up front)

Per the realm-spike conflict experiment, files are **deliberately NOT
pre-partitioned** between sub-TLs. Only shared *artifact* namespaces are
allocated, so plan/ledger writes never collide:

| Artifact | Owner |
|---|---|
| `plans/post-restart/extract-wave.md` | wave TL (extract-wave) — sub-TLs DO NOT edit |
| `plans/post-restart/extract-wave/OPERATIONAL.md` | wave TL |
| `plans/post-restart/extract-wave/boot/**` | sub-TL `boot` (plans + `LEDGER.md`) |
| `plans/post-restart/extract-wave/spawn-latency/**` | sub-TL `spawn-latency` (plans + `LEDGER.md`) |
| `plans/README.md` | wave TL only, at final fold |
| dev-level plan docs | under your own sub-TL directory, numbered `NN-<slug>.md` within it |

**Expected real code overlap** (do not negotiate it away; write minimal,
localized diffs and log it at fold):

- `haskell/app/Main.hs` — `boot` item 0 step 4 (render+loop from ONE extract
  invocation) touches turn-mode emission; `spawn-latency` D1 touches
  `writeWholeModuleClosed`'s metadata merge (~line 348). Same file, different
  regions, both live.
- `tidepool-runtime/src/session/compile.rs` / `turn.rs` — `boot`'s multi-target
  work vs `spawn-latency`'s timing brackets.
- The realm-build lane (a PARALLEL wave, not ours) also lives in
  `tidepool-runtime/src/session/resident.rs`. Its step 4 is HELD on our
  boot-site landing. Conflicts resolve at fold.

## Block

```
### OPERATIONAL RULES (verbatim, non-negotiable)

- Commit with `git commit --no-verify`. NEVER `git add -A` — stage explicit
  paths only.
- The repo-root `tmp/` directory is PROTECTED: never delete or overwrite
  anything under it.
- NEVER run a path-unscoped `pkill -f`. Scope every kill to your own worktree
  path.
- Do NOT run `scripts/redeploy.sh`. Root owns the redeploy at dogfood resume.
  A wire break is already in effect (`--emit-stmt-binders`/`--emit-binders` are
  gone; the deployed pair on this box is the old consistent pair and dogfood is
  PAUSED). Your test runs build the repo extract fresh, so your lanes are
  unaffected.
- GHC slots are box-wide capped at **3** and several waves are running
  concurrently — expect contention.
  - `scripts/battery.sh` and `scripts/battery-shard.sh` SELF-SLOT (they re-exec
    themselves under the broker). Do NOT wrap them.
  - Any OTHER GHC-heavy command (raw `cabal build`, `cabal test`, a bare
    `cargo nextest` over a GHC-extract crate) goes through the broker with an
    absolute path and NEVER exclusive:
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
- `export XDG_CACHE_HOME="$PWD/.cache"` before harness shards.
- NEVER run bare `scripts/battery.sh` — this environment hard-kills background
  processes at ~380s and the full battery is hours. Use:
  - tier 1 `cargo nextest run` (pure-Rust, safe unattended);
  - tier 2 `scripts/battery.sh -p <crate> -E 'test(<name>)'`;
  - tier 3 `scripts/battery-shard.sh <crate>`;
  - tier 4 `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh <crate> ...`,
    deliberately, one suite at a time, outside the ~380s assumption.
  Size every shard for the ~380s kill.
- INHERITED-RED RULE: a "pre-existing / inherited red" claim requires a
  cache-consistent A/B baseline run in YOUR OWN worktree — same compile-cache
  state on both legs, your diff absent vs present. An argument from "my diff
  doesn't touch the failing test's files" is INVALID for global surfaces
  (prelude exports, pragma/extension sets, shared flags): every Haskell compile
  is downstream of those whether or not its file is in the diff. Note the cache
  confound explicitly — a fingerprint-invalidating change makes a naive
  comparison measure cold-vs-warm, not the diff.
- Extractor id-stability is a PINNED invariant (three permanent tests from the
  ConTags incident: `session_table_qualified_identity` plus two quick-tier
  assertions). If your change fires them, STOP and escalate to your TL. That is
  a design conversation with root, not a test to silence.
- RECEIPTS ARE PER-BINARY PASS/FAIL COUNTS, never exit codes. Paste the counts
  (`N passed, M failed` per test binary) in your submit note. "It passed" with
  no counts is not a receipt.
- Never touch another agent's worktree. Never checkout another branch. You are
  your worktree.
- `TIDEPOOL_EXTRACT` must point at a freshly built `tidepool-extract-bin` for
  any test that compiles Haskell; the battery scripts do this for you when it
  is unset. Symptom of a missing one: `Metadata entry must be an array of
  exactly 7`.
```

## Correctness gates (this wave's standard; sub-TLs enforce per item)

- **hardened differential** with its floors — `haskell_suite_differential`
  (`#[ignore]`d + expensive: `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh
  tidepool-codegen --run-ignored all -E 'test(haskell_suite_differential)'`).
  `COMPARED_FLOOR` must not drop.
- **corpus_report** — same shape,
  `-E 'test(corpus_report)'`.
- **extract-fidelity-test 26/26** —
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- bash -c 'cd haskell && cabal test extract-fidelity-test'`.
  26 of 26, no fewer.
- **harness acceptance** — `scripts/battery-shard.sh tidepool-harness
  -E 'binary(/^acceptance_/)'` (shard further if it exceeds the budget).
- **E6 additionally**: the FULL set above, zero tolerance — exposed unfoldings
  change what extraction sees, so a single regression blocks the item.
