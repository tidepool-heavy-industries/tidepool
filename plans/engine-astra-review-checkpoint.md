# Engine/harness checkpoint for independent architecture review

Implementation is paused at the user's request on 2026-09-30. The purpose is a
fresh-context Astra review of the whole workflow and its owning boundaries,
including whether accumulated staging and capability machinery is justified.
Quality takes precedence over historical plan adherence and internal compatibility.
No implementation parcel should resume until the review is discussed.

## Source and preservation

The integrated implementation revision is
`cbc513bc106c80f4a77e70921e9bcc2274b0dcec` in
`/srv/swarm/checkouts/tidepool`. The only root untracked path is the historical
`test-source-boot/`; preserve it. Pushes remain deferred. No live provider run or
default-backend switch is authorized by this review.

The actual harness pin is `9986ca3cf8b1e4be9826cb7420de01e4371922c7`, clean in
`/tmp/tidepool-final-browser-driver`. Do not substitute the old harness master or
`harness-foundation` worktree HEAD. Cargo/flake/Buck pin this candidate.

Retained inventory and binary diffs are in
`target/completion-evidence/final-delivery/astra-review-checkpoint/`.
These are preservation artifacts, not patches to apply automatically.

| Owner | Checkout | HEAD / state |
| --- | --- | --- |
| Compiler | `/tmp/tidepool-wave-next-compiler` | `6bd12591e5d068321ae0440516435c8e412a4fdc`; uncompiled ordered-offer WIP in `artifacts.rs` and `checked_cell.rs` |
| Runtime | `/tmp/tidepool-wave-next-runtime` | `f2ec451e62f7d0c9bfc386c4e2c106b700b05072`; existing modified Codex submodule preserved |
| Actor | `/tmp/tidepool-wave-next-actor` | `0b60bea967af4c5264063607a6ce000dea0a0151`; clean, latest child pipeline not joined |
| Facade/recovery | `/tmp/tidepool-final-facade-recovery` | `d14ead658e25cab4254012493dbbeda16bdcff85`; clean, tree matches integrated `76f2` |
| Performance | `/tmp/tidepool-wave-package-validation` | `df6e4005667230445b8e5ad8f427ed52eab042c2`; clean |
| Scaling comparison | `/tmp/tidepool-wave-final-turn-scaling-index` | `1aface331ae59727846fa584cdf56df1d910d254`; clean |
| Native demand | `/tmp/tidepool-final-root-native-demand` | `600d106f833301dd749e88d2d33954c1e389a7ae`; clean, both leaves joined |

## Intended outcome and review inputs

The target remains a working new engine and opt-in embedded browser-operated Sol
root with recursive Luna worker trees, one harness process per run, real resident
Haskell, independent private executions, atomic declaration/binding publication,
and reusable captures that survive later parent failure. Stock Codex remains a
separate fallback; do not build it with Buck. The resident compiler remains a
runtime service, not a Buck runtime scheduler.

Read AGENTS.md, docs/GLOSSARY.md, plans/engine-harness-final-delivery.md,
plans/harness-first-tree-prd.md, plans/harness-integration-runtime.md, and
plans/engine-harness-integration.md. Reconcile those plans with current production
callers. `plans/engine-final-coverage-de31.md` is an explicitly historical audit,
not current acceptance. In the pinned harness read its contributor rules, NEXT.md
and docs/embedding-ready-handoff.md where present.

Reconstruct end to end: operator input/reconnect; Engine/Store request identity;
immutable tool surface and actor admission; parser/checking producer; reservation;
whole-cell typechecking; private execution/native effects; publication/cancellation;
capture/child release; retirement; durable transfer and restart. Inspect consumers
and failure paths, not only proposed types and new unit tests.

## Evidence and current failures

Full M1/M2/engine acceptance is NOT complete. Candidate source-clear reviews and
compilation do not establish production activation.

- Browser at `c60e08aff1817c0c6b840d19dc5f7eaf6a7526ab`: actual resident Haskell
  returned exactly 42, and exact originating request history progressed to the
  cancellation step. Startup took 228.658s; raw result took 195.684s after browser
  startup. The cancellable cell failed between native snapshot completion and JIT
  completion, before reaching Sleep. Actor exited Failed. This is not a proved
  interrupt/timer defect. The run failed 0/1, 232 filtered, 534.15s test time.
  Log: `target/completion-evidence/final-delivery/browser-request-identity.log`.
  Joined `308fa71b46a3be82bb51853d18f738b1c885e06a` adds bounded actual typed
  failure diagnostics; it compiled but the browser has not rerun with them.
- Cold recovery at tree `0c6a` with immutable compiler pair 8a: two executed tests,
  zero passes. Original declaration publication succeeded, then a stale fixture
  owner assertion failed. Joined `71ccddbf` checks exact canonical SessionModule
  identity; no affected rerun yet. Successor initialization failed after 233.22s:
  native dependencies differ from the original bootstrap initialization.
  Actual order is journal Admitted/record registration -> initialization/tools ->
  facade ApplicationPrepared/transfer. Tools mutate the exact inventory before
  transfer; the strict guard correctly refuses. Packet:
  `/tmp/tidepool-final-facade-recovery/target/completion-evidence/cold-0c6a/`.
- Ten counted extractor-free native demand/cache/CAF/rollback/reclamation tests
  passed using the Buck-built c60 binary. Packet: `native-cache-c60/` under the
  main evidence directory. New late independent demand executes both installed
  groups in one machine, verifies omitted missing dependency/refusal/cache reuse,
  then program retirement and final-reader image reclamation: 1 pass, 542 filtered.
  This does not prove a runtime-discovered dependency or compiler-issued authority.
- Latest joined library/unit-test binaries for facade, runtime, codegen and
  toolchain built successfully (44.232s); `native-perf-build.log`. This built the
  diagnostic WIP bytes later frozen as 308fa; it did not execute those tests.
- Full authentic B100/N1/N10/N100 scaling matrix passed at frozen `7c0` (not final
  source). N100 took 1716.30s; 101 prefix compiles 1495.331s, 100 binds 8.622s.
- Identical B100/N10 fixture across integrated `7c0` -> `1aface`: 366.29 -> 273.50s;
  eleven prefix compiles 162.860 -> 113.020s; native binding roughly unchanged.
  Compiler/hydration changes are also present, so this is not index-only causality.
  `protected-prefix-scale-index/pause-handoff.md` inventories all eight scaling
  gates, exact sources/pairs/binaries, logs and cleanup.
- Component tests prove bounded stage-local package validation/module indexing
  and verified durable existing-artifact reuse. Full 46.4MB original payload
  repeated twice: old temporary writes 92.9MB, new zero payload writes with full
  SHA/file+directory durability checks. Do not attribute all runtime write volume
  to this function. See `plans/engine-host-prefix-validation-cost.md` and owner
  report at the performance checkout.
- The shortened post-index profile has 703 samples, zero lost, includes Rust and
  GHC children; perf command exit 143, not a clean 25s run. Process I/O showed
  2.35GB writes, not explained by explicit interface receipts. Device I/O is
  unavailable because the sampler misresolved the cgroup path. Preserve these
  qualifications; CPU samples and process bytes are not full-run attribution.

## Unjoined/incomplete mechanisms needing scrutiny

1. Final ordered mixed-cell issuer is absent. Parser capability, durable Lib-range
   allocation, ordered Val/display reservations and pre-CAS guard exist on owner
   branches; planned execution stays refused. Whole-cell original recipes must
   typecheck before effects, with exact original Names/identities and independently
   authenticated live completed prefixes. Prepared future Val interfaces are
   type evidence, never future native values.
2. Original Lib declarations importing prior thin Val interfaces need distinct
   immutable compiler-owned interface custody through certificate/context,
   publication and hydration. Current Home/Join custody does not model this.
   Lost native values after restart remain unavailable; no effect/source replay.
3. Current-producer fencing and initial configured-producer authority must remain
   distinct. Observing a worker SHA is not itself deployment authorization.
4. Child pipeline 0b60 retains original owners/lexical leases, initializes before
   boot, distinguishes visible Unconfirmed from Durable, and uses tagged
   confirmation-only Resume retries. Sixteen focused tests passed; no real native
   child/provider acceptance. Native boot and other paths remain Serial.
5. `pending_task` is still one Option; keyed admission disabled. Central cursor,
   ordinary Resume, ReleaseFork defaults and numerous effect/ingress continuations
   still use Serial. Private runtime primitives are not full actor concurrency.
6. Proposed cold repair: distinct pending-root admission after actual journal/record
   registration, opaque original startup-release owner, facade sealed transfer/
   Store CAS, then confirmed durable initialization/boot/tools/readiness. It is a
   proposal, not implemented. Review whether this is the simplest correct owner
   boundary, including drop, retirement, uncertainty and re-entry.
7. Typed installed tools through Engine, real late output/compaction/host loss,
   complete cancellation/publication ordering, capture children before parent
   return/later failure, structural corpus/embedded producers/cache invalidations,
   package startup and fresh milestone reviews remain gates.

## Requested review output

Return a grounded architecture evaluation with severity-ranked findings and
owning file/caller references; distinguish reproduced defects, source-proven
contradictions, unvalidated designs and open acceptance. Identify unnecessary
owners, duplicated representations, conversions, scans, state variants, sequential
fallbacks and format obligations. Compare simpler alternatives against full
workflow correctness, failure isolation, latency/memory/build cost. Recommend a
concrete sequence through completion and which current parcels to keep, simplify,
replace or delete. Do not edit source, launch broad builds, push, or run providers
as part of this review. Read-only source/evidence inspection is authorized.
