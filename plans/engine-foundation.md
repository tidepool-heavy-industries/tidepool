# Compiler, code lifetime and external execution foundation

Approved implementation, 2026-09-28. Base: `1d10fac72d948adba75b9fe5bf47360e0701d0de`.
The wave22 investigation found three coupled mechanisms: unrelated retained
symbols multiplied interface fingerprint work; run-owned image references
prevented native code reclamation; external operations held shared machinery
and delayed control delivery. Fix the owning mechanisms, not timeout symptoms.

## Contracts

- Preserve one active notebook operation per actor and current publication
  semantics. Prepare explicit execution/continuation ownership for the future
  concurrent notebook contract in `harness-integration.md`; do not enable it here.
- Compiler context is immutable per request and scoped per module at every
  compilation entry point. Unrelated retained symbols cannot change a module's
  fingerprint or multiply its interface work.
- Generated value identities are reserved before any compiler can write their
  artifacts. Rejecting a stale result cannot undo an overwritten interface.
- Immutable compiled code and mutable installed instances have different
  owners. Sharing code must not share imports, globals or evaluatedness.
- Weak code indexes do not keep programs alive. Values, continuations, active
  compilations, installations and parcels retain the code they actually need.
- External work owns its request, authority and completion identity. Release
  the machine before I/O; resume only the exact parked continuation. Cancelling
  a waiter does not prove a subprocess or blocking call has stopped.
- Repository observation does not delay inbox delivery. Compatible reads may
  overlap; mutations and coherent source capture retain proper exclusion.

## Implementation ledger

All unchecked work remains required; early repairs are not completion of the
foundation. Use isolated Sol Medium parcels, exact-commit review and one
expensive compiler/test slot. Preserve shared daemons and unrelated worktrees.

### First repairs

- [x] Index retained symbols once per request; scope custom prepared-interface
  construction as well as ordinary GHC load; repair `NoQuasiQuotes` memo reuse.
  Integrated: `9f2aa1deb` (`foundation/compiler`, candidate `3fbac7d35`).
  Focused production-interface regression passed; removing saved-environment
  scoping made it fail. Warm memo trace: three modules, zero typecheck/lowering ms.
- [x] Weak live image registry, dead-key cleanup and real-owner lifetime tests.
  Integrated: `9c4f92c15` (`foundation/image-lifetime`, candidate `fd6acc569`).
  Four registry tests passed, including last real machine-owner release.
- [x] Independent bounded observation loops and inbox delivery, skipped missed
  ticks, honest shutdown/resource retention. Integrated: `a2c82aa07`. Four
  production-supervisor barrier tests passed, including blocked observation and
  unfinished blocking work at retirement. Source freshness remains separate below.
- [x] Reserve folded-cell value identity before off-checkout compilation.
  Integrated: `dafd3112a`; four actor generation/split-install tests passed.
- [ ] Restore hosted Codex call terminal settlement and waiter notification.
- [ ] Represent failed/stale source observation honestly instead of presenting
  an earlier successful observation as a current check.

### Reusable compiler and native artifacts

- [ ] One module product owns compiler/source/dependency identity, paired
  interface and prepared definitions, exports/imports and reachability evidence.
  Extend the existing artifact owner; do not add a second cache.
  Worker-local paired products integrated in `0cc562a14`; five focused Haskell
  entrypoints passed. Durable toolchain-qualified artifacts remain unfinished.
  Plugin marker scoping correction integrated in `151ad4f7a`.
- [ ] Distinguish compile-time Template Haskell execution from reusable runtime
  output. Untracked compile-time effects remain uncached.
- [ ] Demand native fragments by recursive binding group; batch missing groups
  per module version. Retain prepared definitions without eagerly compiling all
  unused code. Avoid one allocation owner per function.
- [ ] Separate immutable fragments from installed roots/imports/callable
  descriptors. Pass installation environment through the native entry ABI;
  remove compile-time process-wide root-table slot identity.
- [ ] Support distinct installations of shared code, including different
  captures in one machine. Preserve old instances through inherited values,
  checkpoints and parcels. Environment-dependent closures are instance-owned.
- [ ] Exact indexed code identity and duplicate-compile coalescing. Keep mutable
  runtime state out of code identity. Reclaim abandoned candidates and dead keys.
- [ ] Migrate artifact schema/execution ABI through their producers and regenerate
  embedded artifacts; no compatibility shim for the superseded internal boundary.

### External execution and repository admission

- [ ] Owned deferred request/completion protocol, exact late/duplicate fencing,
  machine release during external effects and responsive actor control delivery.
- [ ] Move filesystem/Git/process/network/timer work across that boundary;
  keep machine-local operations local and preserve notebook admission semantics.
- [ ] Thin namespace launcher under the existing process owner avoids forking
  Git from the large JIT host. Preserve descriptor authority and cleanup.
- [ ] Read/write/capture admission in the Git owner replaces blanket reentrant
  exclusion. Preserve stable sampling for native writers outside those locks.
- [ ] Coalesce source/view observation at its owning boundary; distinguish
  cached, fresh, pending and unavailable observations.

## Acceptance evidence

- Matched tiny-module compilation with 0/1,000/10,000 unrelated retained symbols;
  exercise production prepared-interface construction, not only ordinary load.
- Dependency invalidation, shadowing, reload, private exports, recursion and TH.
- Compile once, install twice with different imports; producer retirement while
  received closures, parcels or parked continuations remain live; final-owner
  release actually frees native allocation.
- A blocked Git operation does not hold unrelated machine/control progress;
  cancellation, late completion, stale compilation and installation failure keep
  custody and cleanup correct.
- Focused tests for each parcel, full prepared fixture boundary, then matched
  local build and integration gate. Do not launch a successor wave from this batch.
- Report live and cumulative native bytes separately, observation/admission/hold
  timing and compile reuse. Virtual mappings are not resident memory.

## Retained investigation

Wave22: `fc8dae5b-b02e-4884-959c-12cc060b1b8c`; source
`5e4ba833af8841a7e0f47714fcf2e8321f1e0440`.
Matched probe evidence: `/tmp/tidepool-wave22-fullcore`.
For the same tiny generated module, adding 10,000 unrelated retained symbols
increased interface allocation from below the RTS counter resolution to about
679 MB, and interface time from 0.322 ms to about 529 ms. The full 23-interface
request allocated about 15.9 GB. These measurements establish the fingerprint
defect; they do not measure the benefit of changes that have not yet run.

The retained old-worker `Tidepool.Session.Val.G3` `make_iface` detail lines in
`/tmp/tidepool-wave22-fullcore/retained-{0,1000,10000}.log` give the exact
baseline below. These are per-module process-delta counters, not whole-request
RSS. The W1 input and G3 source are preserved byte-for-byte in
`tidepool/extract-cmd/tests/fixtures/retained_fingerprint/`.

| Unrelated retained identities | G3 wall time | G3 allocated bytes |
| ---: | ---: | ---: |
| 0 | 322,328 ns | 0 (below counter resolution) |
| 1,000 | 23,119,055 ns | 68,006,680 |
| 10,000 | 526,178,312 ns | 679,149,432 |

The historical request's `G2.hi` has no retained source (only a 291-byte
interface tied to that old worker), and W2 imports private wave Project
modules. The portable, ignored `retained_fingerprint` integration probe uses
the typed `ExtractCmd` producer to make a fresh G2 thin interface, then runs
the same W1/G3 sources against 0/1,000/10,000 unrelated identities. Its
minimal local `Tidepool.Command.Types` and G2 binder replace missing wave
dependencies; compare the three new rows to each other, not their absolute
times or allocations to the historical rows. A single matched run on the
current worker produced:

| Unrelated retained identities | G3 wall time | G3 allocated bytes | Whole request elapsed |
| ---: | ---: | ---: | ---: |
| 0 | 178,481 ns | 0 (below counter resolution) | 101,983,457 ns |
| 1,000 | 175,808 ns | 0 (below counter resolution) | 116,388,350 ns |
| 10,000 | 200,776 ns | 0 (below counter resolution) | 309,163,266 ns |

The G3 prepared-interface phase did not scale with these unrelated identities
in this one run. The whole request still grows, so this does not measure a
whole-request speedup. The probe ran in the repository Nix shell with
`TIDEPOOL_TIMING=1`, this checkout's freshly built worker and frontend, and
`cargo test -p tidepool-extract-cmd --test retained_fingerprint
unrelated_retained_symbols_do_not_scale_g3_interface -- --exact --ignored
--nocapture` (1 passed). The old and current rows use different G2/dependency
fixtures and worker revisions; their absolute times are not a controlled
before/after speed comparison.

Long shared-machine holds, serial Git/process setup and blocked observation
loops amplify each other. The controlled fork experiment confirms a large
mapping/resident-memory cost, but does not attribute all production wait time
to fork. Continue measuring at the owning boundaries after each change.

## Active integration notes

The final Codex settlement candidate is
`c09c2b067774be4104fad878ee271a6a14f12690`. Its source-box focused run passed
13/13 after repairing stale test APIs, registration and fixture drift. The
destination candidate pins that commit; destination verification is recorded
in `engine-foundation-transfer.md`. No running client or service was replaced.

The destination join includes the transferred foundation, module-product,
adapter and transitive binding-retention candidates. Root owns integration;
bounded agents own ABI validation, compiler publication proof and companion
review. Parallel checks use separate mutable outputs and explicit memory
limits. Drafts, target compilation and executed tests remain distinct evidence.

## User review boundary

The 2026-09-28 review is complete. Implementation of the remaining parcels is
authorized under [engine-harness-integration.md](engine-harness-integration.md),
including the concurrent resident follow-on. The earlier hold below records the
boundary that led to that review; it no longer blocks the approved batch.

Inanna requested an explicit yield before the next major design review.
Finish and review the active fixes and their focused verification, including
constructor identity, code lookup/coalescing, external execution, actor control,
client settlement and source observation freshness. Do not begin implementation
of either remaining design parcel before that review:

- Durable module artifacts and demand-driven native compilation.
- Git admission and the thin namespace launcher.

When the active work reaches a reviewable handoff, flag that the review point
has arrived and yield to Inanna, who will set up the higher-effort review.
Present current evidence, remaining decisions and curated alternatives. If an
active fix exposes a consequential unresolved design choice sooner, flag it
and yield at that point instead. These parcels remain required in the overall
foundation plan; this boundary changes sequencing, not the intended outcome.
