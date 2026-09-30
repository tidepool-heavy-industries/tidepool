# Engine / harness checkpoint for Codex update

2026-09-30 UTC. User requested orderly wind-down to update Codex and resume with
Sol 6.1. Do not restart workers automatically during this checkpoint. No push,
live provider trial, or backend-default switch is authorized by this handoff.
Codex must not be built with Buck.

## Source and retained evidence

- Main/Buck: `/srv/swarm/checkouts/tidepool`, `b11bf22f6f` before this document.
- Joined product: `/tmp/tidepool-product-integration`, `76ce77921a`.
- Snapshot: `target/completion-evidence/update-handoff/20260930T042536Z`.
  Contains exact heads, binary patches, untracked source, worktree inventory,
  selected gate logs and the pending nominal checked-pin proposal. The `latest`
  file in its parent identifies this snapshot. Actor work may receive a final
  refreshed patch/handoff after this initial snapshot; consult its final status.
- Product dirty files are opt-in diagnostics in resident_actor, command_settlement,
  resident_local_actor test, heap raw/promotion, and runtime prepared. Preserve
  them; do not overwrite entire files when joining candidates.
- Main is the Buck baseline; product is the engine/harness integration baseline.
  They are not yet the same accepted product. Preserve all linked worktrees.

## Accepted in this wave

- Protected compiler producer `8147fbccb`: actual source-hidden original products,
  downstream G4 frontdoor and cache-evidence refusal, one executed test PASS.
  Haskell declaration/cell-splitter and Rust compile checks passed earlier.
- Native export ownership `7722589f7` + `b85147e0c`, joined `47bee6df3`:
  exact live native roots distinct from lexical bindings, two tests PASS.
- Export staging `88dee24ae1`, joined `2d6812597`: stage all fallible roots
  before publication; two isolated tests PASS. Joined runtime four tests PASS.
- Captured child candidate `9f359958`, joined `8390bfcb0` + fixture correction
  `76ce77921a`: three actor admission and two facade ancestry tests PASS.
  The stronger same-unfinished-parent-cell failure fixture compiles. Actual
  captured host execution is NOT accepted.
- Harness InputObserver `abfbf3b3`: native test PASS. Product pins it through
  `0d375d9fac`; observer narrowing and retirement fencing joined through
  `245b418b17`. Their real notification tests remain blocked before assertions.
- Main native Buck bundle builds, startup smoke and actual packaged Haskell
  compilation both PASS. Shared library closure now comes from the Prelude
  provider (`a548454dd8`, joined `b11bf22f6f`). Exact local Git source override
  is restored after gates; canonical remote fetching remains unverified.

## First resume actions and pending candidates

1. Join and gate `458fecd7a5e0b00eaff691b129076dc50d623643` from
   `/tmp/tidepool-settled-pair`. Actual notification diagnostics proved Done and
   Suspended exist in separate admitted native groups, while old aggregation
   required a complete pair in one group. Candidate joins full constructor and
   family identities and rejects conflicts; formatting only, no tests yet.
   Its constructor tuple gains a family field: update product's uncommitted
   diagnostic tuple patterns. Run focused settled test, then real notification
   and watch batteries. Logs: `joined-settled-diagnostic-gate3.log`.
2. Runtime exact consumers: `/tmp/tidepool-runtime-exact-consumer`, committed
   `214469d824` (consult actual HEAD for final handoff). Includes producer8147
   and diagnosticbbde plus runtime consumers and explicit admitted lexical
   surface graph. Gate3 executes three tests: paired publication two PASS;
   source-hidden consumer FAILS only at strict same-check fold assertion.
   It reaches hidden-original refusal, mixed/shared/rejected inspection, type
   batch and successful check. Fold/separate execution remain unaccepted.
   Compiler `/tmp/tidepool-compiler-owner` clean at `9a42cfe8d7` adds bounded
   synchronous fold exception diagnostics (async exceptions rethrow). Combine
   that one-file commit with runtime's missing-fold evidence retention, rebuild
   matched worker, rerun only source-hidden consumer with KEEP_TEST_LOGS=1.
   Diagnose the actual fold exception; do not weaken its assertion.
3. Actor park/resume: `/tmp/tidepool-actor-park-owner`. Newly started partial
   scheduler WIP, no acceptance. Obtain its final handoff. First production
   parcel moves preparation and captured awaitWatch through typed nonterminal
   owned completions; retains single hosted admission. Transfer exact cleanup
   claim, source/tool leases and explicit task-local continuation owner; fence
   each step generation. Other external waits and true A/B concurrency remain
   later work. Do not enable multiple admissions before private/publication
   authority closes. No second scheduler or ContinueLater shortcut.
4. Buck cache gate: `/tmp/tidepool-buck-product`, commits `ee18fbcace` and
   `1fc98196cd`. Action-identity cache probe and signal/concurrent-edit-safe
   restoration reviewed; no actual Buck cache gate yet. Root admits it with
   exclusive generated BUCK ownership. Docs-only audit commits `674fdc3a8e`
   and `ea1eae7209` also await review/join.
5. Observer graph pin: `/tmp/tidepool-harness-buck-graph`, clean `6a3bceb2f2`.
   Equivalent pin already exists as `8b850206a`; don't apply both. Regeneration
   stopped on `Unmigrated local dependency: codex-shoal-protocol` with default
   feature selection. Investigate embedded-only generator selection first;
   do not solve this by adding Codex Buck targets. No graph changes or Buck
   build from this candidate. Nix hashes were verified against exact local Git.
6. Optional quality candidate: `/tmp/tidepool-embedded-warning-cleanup`,
   `b2ff81600b4f1f4f38c0c65710a3e935e71ef3c5`. Three facade files gate legacy
   helpers and remove an unused prompt field. Format/diff checks only; both
   default and no-default compile gates required. Larger dead-code cuts were
   not made without consumer evidence. Queued historical docs reconciliation
   was cancelled before initialization; no result should be inferred.

## Outstanding completion contract

M1 sequential browser/host acceptance, M2 concurrent private executions and
atomic publication, reusable independent captures, nominal checked-pin
metadata authority, durable recovery of admitted lexical surface, full structural
fixture corpus, final joined Buck package and cache gates all remain open.
The nominal proposal is copied into the snapshot as compiler-checked-pin-next.md;
it requires preimplementation proof that abstract imports cannot leak instances
or names. Current passing focused checks do not establish full completion.

## Build ownership on resume

Use pinned `bash scripts/dev-shell.sh` (Rust1.93/GHC9.12.2), battery wrapper for
actual Haskell tests, explicit package/target/filter and executed counts. Root
admits work through user `tidepool-completion-build.slice` (104 GiB aggregate).
Use separate Cargo target directories for divergent worktrees: sharing one
produced incompatible stale repr evidence in this wave. Do not clear caches.
Buck remains local-only with `-c remote.enabled=false`, mounted buck-out and
owned existing daemon. Preserve Nix/compiler/shared services; no daemon restart
is needed for this update. No root-owned build gate was running at wind-down.
