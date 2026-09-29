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

Long shared-machine holds, serial Git/process setup and blocked observation
loops amplify each other. The controlled fork experiment confirms a large
mapping/resident-memory cost, but does not attribute all production wait time
to fork. Continue measuring at the owning boundaries after each change.

## Active integration notes

The Codex settlement repair is in the isolated `foundation/client-settlement`
worktree. Its first focused test compile exposed twelve stale test API calls,
and the existing `host_input.rs` regression file was not registered. Those
repairs and the new response-only terminal regression are drafted; no client
tests have executed yet. The pinned client and running services are unchanged.

Three Sol Medium lanes are implementing installation environments, owned
external effects, and responsive actor execution. Root owns cross-layer review,
actor effect adaptation, client repair, and integration. Builds use one slot;
drafts and successful formatting are not verification.
