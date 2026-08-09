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
- **THROTTLE DIRECTIVE (root, 2026-08-08 — IN FORCE until root lifts it).**
  The box hit load average **92**: the operator's SSH sessions died and a dev
  pane died in the same window. Seven concurrent `tidepool-extract` compiles
  were observed — nextest's `ghc-heavy` cap is **per-run, not box-wide**, so
  parallel worktrees multiply it. The 3-slot semaphore is now the box-wide
  governor for ALL heavy work, not just GHC-extract work.
  **Wrap EVERY heavy invocation** in the broker, absolute path, NEVER
  exclusive:
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
  This now includes, and did not before:
  - `cargo nextest run` — **ANY tier, including the quick pure-Rust tier**
  - `cargo check --workspace`, `cargo build --workspace`
  - `cargo clippy --workspace`
  - anything spawning extracts outside the battery scripts
  EXEMPT: single-crate `cargo check -p <X>`, file edits, greps.
  `scripts/battery.sh` / `scripts/battery-shard.sh` already self-acquire —
  do NOT wrap them; that is unchanged.
  If a slot wait exceeds ~15 minutes, REPORT it upward as a starvation signal
  rather than bypassing the broker.
  **MEASURING actual load — the obvious commands are both wrong.**
  `pgrep -fc tidepool-extract` OVER-counts wildly (22–24 on a box with 4 real
  compiles): it matches every agent shell, `.claude-unwrapp`, `bash`, `zsh` and
  `timeout` process carrying `TIDEPOOL_EXTRACT=…` in its command line or
  environment, so it tracks how many AGENTS exist, not how much GHC runs.
  But matching on `comm` with the full binary name UNDER-counts to a constant
  zero: Linux truncates `comm` to 15 characters, so `tidepool-extract-bin`
  appears as `tidepool-extrac` and `grep tidepool-extract-bin` can NEVER match.
  The correct instruments:

      ps -eo comm= | grep -c '^tidepool-extrac'    # real compiles (note: 15-char truncation)
      cat /proc/loadavg                            # actual load

  Use both. A zero from a mistyped pattern reads exactly like a quiet box.
  **Bias toward fewer, better-batched runs.** This wave is the heaviest GHC
  consumer on the box, so the throttle bites hardest here.
- `export XDG_CACHE_HOME="$PWD/.cache"` before harness shards.
- NEVER run bare `scripts/battery.sh` — this environment hard-kills background
  processes at ~380s and the full battery is hours. Use:
  - tier 1 `cargo nextest run` (pure-Rust) — **MUST be broker-wrapped under the
    throttle directive above. It is NOT "safe unattended"; that phrasing is
    WITHDRAWN as of 24f6d7a7.**
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
- NAMED-GUARD RULE (wave-wide, from spawn-latency, 2026-08-08): **if a gate
  exists to catch ONE specific failure mode, the receipt must show that
  specific test passing by name, with its own pass line — not the aggregate
  that contains it.** An aggregate count proves a suite ran; it does not prove
  the guard executed. The residual hole it closes: the test is present in the
  tree but the shard's filter does not select it — a renamed binary, an
  `#[ignore]`, a cfg, an env-gated early return. Then the base commit is
  correct, the count is green, and the guard never ran.
  Where a guard is CROSS-LANE (it lives in one lane's branch and protects
  another's change), the receipt must ALSO name the base commit it ran
  against. Base proves which tree ran; the test name proves execution. Both,
  or neither is established.
  This is the same instrument that produced the C1 finding: do not trust that
  a label ("harness acceptance, N passed") covers what its name implies.
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
