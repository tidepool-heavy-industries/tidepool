# Engine completion and embedded harness delivery

Approved 2026-09-29. This sequences the accepted engine completion contract in
`engine-completion-next-wave.md` with M1-first harness delivery. Quality takes
precedence over compatibility and earlier implementation choices. Migrate
consumers instead of retaining weak internal boundaries. Persisted formats need
explicit migration/refusal. This document does not claim M2 acceptance.

## Starting evidence

| Owner | Checkpoint | Evidence / limit |
| --- | --- | --- |
| Joined Tidepool | `5bf38a34e2f89666c732f12bf3807b67825ffb0b` | Earlier joined native/host 11/11 at `bef801b6c`; later exact execution fencing integrated. |
| Compiler | `26e17afb6f13a370460ba8e0f14e48ac700d8ced` | Real module hit/miss and source-hidden proof pass; resident certification and recursive reuse unfinished. |
| Native | `1ddb871d88b3f956d4c65bd85c36554981331305` | Exact-owner runtime 2/2, demand 2/2; production resident consumer unfinished. |
| Runtime | `c8d13763491952eb178dbed7352d282a5550c7f1` | Wrapper 3/3, recovery codec 7/7; publication/concurrent execution/v2 adoption unfinished. |
| Declaration validator | `283ef49cc2dfffd2ac5e20b91d1245ab644029e0` | Home isolation and actual fresh worker consumers pass; package isolation unproved. |
| M1 | `3606f16096468e7ee4a17b17401b1a9b776ce67c` | Real-cell and pending-call/compaction/cancellation checks; successful complete application-host test still missing. |
| Packaging | `7b4525710719df0018b8c5c9a539f1aaea69dd91` | Exact-source package built with immutable browser assets; not final joined acceptance. |
| Harness | `c485edb9b697ffc671b22c9ef25a73fc84763d76` | Existing adapter pin, ten commits after `f495e93`; identity/schema-v5/admission/cancellation changes must be verified together. |

Compiler, declaration and runtime WIP is retained in the existing isolated
worktrees. Exact tracked/untracked inventories and patches are retained under
the dated completion evidence directory before integration. Preserve worker
branches and source evidence; integrate reviewed commits without duplicating
equivalent cherry-picks. Earlier standalone harness test totals do not establish
acceptance of the ten later commits.

## Ordered parcels

1. **Harness contract / M1 baseline.** Join reviewed host tests and packaging
   onto the sequential engine baseline. Start harness changes from c485edb9.
   Keep qualified OperationId and schema-v5 migration together. Remove embedded
   wait_agent injection/exemption; preserve standalone behavior and existing
   final-with-pending internal wait, nonfinal async progression and durable wake.
   Pin any repaired library and assets to the same reviewed revision.
2. **Full application-host M1.** Facade owns a private deterministic transport
   test seam. Exercise readiness, authenticated browser command, actual resident
   Haskell, retained output, reconnect/history and identical command retry without
   execution replay, then retirement. Derive manifest and dispatcher from the
   same request-pinned endpoint; remove stale installation declaration caching.
   Test reload, failed reload, real cancellation, input inclusion and host loss.
   M1 is explicitly sequential; no child/capture or atomic-cell claims.
3. **Certified compiler/native production.** Worker/toolchain own exact products,
   source/boot/interface/package evidence and explicit certification outcomes.
   Every authored/Join request receives exact retained versus lexical scope refs,
   selected inventories and sealed direct package roots. Missing evidence is not
   empty evidence. Prove recursive reuse with boot and external dependency checks.
   Full package isolation includes forced lazy metadata loads; if an interface
   callback cannot enforce it, use a pinned pre-publication loader hook rather
   than weakening isolation. Check the full retained family closure plus new
   imports/local families on every authored compilation and Join.
   Runtime resolves only demanded retained imports against the admitted lexical
   scope. Preserve full source owner/ordinal through sealing and inherited group
   reuse, including late-demanded sibling binders. Compile target/new groups off
   checkout; install, scope-register and publish metadata atomically. Bootstrap,
   split preparation and stale retries use the same path. Required certification
   failure may retry fresh compilation once, never downgrade to legacy install.
4. **Private execution/publication/recovery.** Execution records own private
   scope, final writes, continuation/control/receipts and admitted source/tools.
   Complete exact Join adoption and paired visibility including hidden-host
   policy/epochs. Reserve durable sparse IDs and burn failures. Stage proof,
   artifacts and manifest outside checkout; restage stale successes/rejections
   without effects. Manifest rename is commit authority, followed by infallible
   visibility swap. Post-rename fsync failure is published/durability-unconfirmed.
   Wire v2 attach/publication/import restoration/tombstones and explicit per-actor
   durable manifests. No heap/token/effect replay. Enable execution interleaving
   only after publication/cancellation and multi-execution tests pass.
5. **Captures and embedded children.** Carry exact origin OperationId through a
   narrow host capability into capture. Existing ForkGroupRegistry retains the
   opaque harness checkpoint; no second registry/supervisor. Reserve actor state,
   retain exact scope/admitted source, commit Store capture before returning the
   token, then publish its combined lease before dispatching the next effect.
   Failed delivery drops capability and retains exact cleanup custody; historical
   rows are unavailable, not resurrectable state. No DB transaction spans Haskell.
   Existing actor installation attaches children via Conversation::from_checkpoint.
   Distinguish transcript origin, creator and supervisor. One shared Store,
   scheduler/server per run, one model-round owner per conversation. Released
   tokens reject new admissions; existing children and successful captures survive
   issuer failure. Browser includes actual Haskell-only workflow actors.

## Acceptance

- G0: exact reviewed engine/harness/assets/workspace pins, migration/refusal,
  changed-consumer compilation, existing Codex fallback checks.
- G1: complete host browser-to-real-Haskell path, exact replay-free reconnect,
  durable input inclusion, pinned reload, cancellation and retirement.
- Compiler/native: resident certified hit/miss, recursive/boot closure,
  source-hidden fresh consumers, package/family isolation, unused-code omission,
  exact CAF reuse versus distinct private instances, late rollback and final-owner
  reclamation. A compiler smoke test is not certified resident demand acceptance.
- G2: A parks/B publishes/A resumes without erasing B; completion-order shadowing,
  old captures, invalid joins, failure nonpublication, stale proof restaging.
- G3: two children use a capture before parent completion and survive later
  parent failure; separate checkouts, one original pending-claim settlement,
  exact release/admission behavior.
- G4: two parked executions plus progressing third/control; both cancel/commit
  orders, stale completion, pre/post-rename faults, exact recovery/lost-head
  tombstones, retained unconfirmed cleanup.
- Final offline: every just verify constituent, required producer regeneration,
  harness workspace/lint/web gates, matched package and Codex regressions at the
  joined revision. Retain source hashes, commands, executable counts, exit status
  and logs. Keep mock, resident, browser protocol and live evidence distinct.
- G5 remains a separately authorized live trial: browser Sol root, recursive Luna
  component work, Haskell result routing, exact review, repair, integration,
  resource disposition and root interview.

## Parallel execution and delivery

Use up to eight workers around meaningful owners. Sol owns compiler, declaration
validation, native and runtime joins; Luna owns bounded harness/M1, recovery,
packaging and reviews. Root owns contracts and integration. M1 proceeds while
engine work continues. Use isolated worktrees for simultaneous edits. Main/master
may receive verified integration; no branch-preservation requirement remains.
Push verified dependencies then Tidepool once the user configures Git auth.

Concurrent builds use the accepted completion slice; there is no blanket single
compiler slot. Expensive Nix realizations remain serialized at cores=2/max-jobs=1
because the daemon has its own memory cap. Do not restart shared daemons. No live
launch, running-session migration or default-backend switch follows from tests.
