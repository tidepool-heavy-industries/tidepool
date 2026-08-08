# Post-restart agenda (written 2026-08-09, during the quiesce)

The swarm was drained to zero active TLs so exo could restart with cgroup
confinement (swarm.slice) baked into its spawn path. This directory holds
everything the respawned lanes need; nothing here depends on any session's
context window.

## Root's sequence, in order

1. Fold any straggler READYs (receipts: per-binary counts, never exit codes;
   `--no-fail-fast` everywhere a known red exists).
2. Rebuild the shared extract at the final tip (the batch changed
   `haskell/tidepool-extract.cabal` — stanza → test-suite) and re-announce
   its path to lanes whose branches don't touch `haskell/`.
3. `scripts/redeploy.sh` + fresh-process smoke (merge/deploy checklist).
4. Composed-tree gate: [`gate-runbook.md`](gate-runbook.md).
5. Respawn lanes, each from its spec file here:
   - [`harness-lifecycle.md`](harness-lifecycle.md) — golden_path triage FIRST
   - [`extract-wave.md`](extract-wave.md) — after Phase B lands
   - [`jit-chain-2.md`](jit-chain-2.md) — resumes cluster B, then D/E/F
   - Phase B TL — spec is [`../one-spawn-turn-protocol.md`](../one-spawn-turn-protocol.md)
     (self-contained; carries the redeploy requirement for the `--emit-*`
     flag removal)
6. Dogfood launch (release-profile binaries) once the golden_path verdict
   and the open-intermittent triage below permit.

Also queued: retire-or-wire decision on the observatory-orphan API cluster
(Harness::first_operator_hole / live_turn / pending_dialog_ui /
tree_snapshot / tree_snapshot_page — pub, zero callers; architectural
call, fits harness-lifecycle's scope).

## Open threads that gate or shadow the dogfood

- **finalize-template ambiguity — cause RESOLVED (2026-08-08), fix in
  flight (`finalize-template-pin`).** The ambiguous-`ToJSON a0` on the
  template's `toJSON _r` is GHC's defaulting ANCHOR rule: defaulting
  fires only when the constraint set contains a standard-class anchor
  (numeric, or Show/Eq/Ord under `ExtendedDefaultRules` — a relaxation of
  the anchor requirement, never its removal). A solitary user-defined
  class (`ToJSON` alone) is never eligible; the default list's contents
  are irrelevant. Discriminator: plain `IO` fails identically to
  `Eff <row>` — refuting d82cf099's recorded hypothesis (untouchability
  under the row implication), which this entry supersedes. Fix direction:
  supply an ANCHOR (e.g. a `Show` constraint riding alongside in the
  pinned wrapper) rather than a hard type pin — additive, keeps bare
  `askUser` turns compiling; hard pin pre-approved as fallback. The
  prompt's annotated-shape stopgap (d82cf099) is correct either way.

- **Intermittent garbage con_tag** (`selfharness_compaction`, open):
  `YieldError::UnexpectedConTag` with a raw-pointer-shaped tag + fast-abort
  (65.7s vs 193–201s band), one sighting under peak memory pressure, same
  tree passed and failed. Sole live discriminator: the bounded GC_POISON
  run — NOT YET RUN (stopped at the quiesce boundary); recipe committed in
  `../self-iterating-harness/12-robustness-wave-receipt.md` (revert
  9f2e18a5 to restore cb1b131d; GC_POISON + HEAP_VERIFY + MAX_HEAP=16MiB
  on the one compaction test). ROOT runs it post-restart, one attempt,
  before the dogfood go/no-go. NOTE: NurseryExhausted is EXPECTED noise in that
  run; only the garbage-tag signature counts. If it reproduces, the fix
  gates the dogfood.
- **NurseryExhausted class, REOPENED**: cc1a86c0 fixed the Eager-response
  `value_to_heap` site, but the nested-child/stowed-continuation path
  failed again under load. Successor audits ALL value_to_heap-class sites
  on that path and extends the gc-retry pattern — no whack-a-mole.
- **Prune re-land held** (`cb1b131d` revert at 9f2e18a5): exonerated by all
  evidence, but re-landing shifts allocation profiles; re-land only after
  the poison verdict, with jit-chain's populated-session gate test, and the
  honest receipt line: "suspected on symptom class, not reproduced;
  reverted out of caution during a release window."

## Small decided items

- `FromJSON ()` flips to aeson 2.x relaxed semantics (Inanna's call:
  prefer 2.x where more relaxed). Instance + probe + haddock.
- Swap recovery on the box: `sudo swapoff -a && swapon -a` once load < ~10
  (swap is at 0.0 free; MemAvailable is healthy — this restores the crash
  cushion, nothing else).
- exo wishlist confirmations from this campaign: silent child death
  occurred TWICE (a TL, then a leaf) — detection is priority 1; merge
  teardown half-fails on already-pruned worktrees (1.1G orphan dir cleaned
  by hand); stale orphaned panes from the first TL death still need reaping.
- exo wishlist NEW (2026-08-08, post-restart): **duplicate spawn / silent
  child birth** — spawn_dev produced TWO panes running the identical
  generated spec in the SAME worktree with ONE ledger entry; the stray was
  invisible to ListAgents and the peer list (its messages arrived
  from-mode=bypass), and was caught only because the ledger dev noticed
  edits it hadn't made. Two concurrent writers interleaved uncommitted
  edits in one file before containment (WIP-commit + freeze + root killed
  the stray by verified PID). Spawn should be idempotent per worktree, and
  a pane whose registration failed should die loudly, not run the spec.
  Corroborating residue (same incident): (a) the dev node's
  `status.children` blob was cross-written by the twin — it listed three
  TOP-LEVEL lanes as children of a leaf dev (ledger tree structure itself
  stayed correct); harmless today but load-bearing if anything ever walks
  `status.children` for teardown/traversal — status files must be
  single-writer or last-write-detected. (b) Post-kill, `ListAgents`
  dropped the (live) ledger child for a window while `tree` showed it
  alive — peer list and ledger can disagree transiently, so the watchdog
  must not read either alone as death evidence.
