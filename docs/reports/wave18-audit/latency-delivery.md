# Wave18 bounded audit: latency and delegation/delivery

Read-only supplement to `turns-helpers.md` and `coordination-confusion.md`. Reuses their trace inventory, coordination/evidence observations, and `/home/inanna/dev/tidepool/docs/reports/wave18-audit/jev-cost.md`; it does not repeat the Jev cost audit. Reproducible counts are in `audit-counts.py`. No build, source edit, or live actor message was made.

## Coverage and counting rules

- Host log: `.exomonad/logs/29e61b63-2bc9-45d5-b5d1-b67e20918bea.log`, fixed prefix of the first **133,731 lines**, last record `2026-09-27T02:19:49.312207Z`. The log continued to grow after that snapshot; later records are outside these counts.
- Timing denominator: **1,608** parsed `call timing` records within that fixed prefix; **458** had `total_ms > 10,000`. These are per workbench call records, not unique model rounds. The fields `checkout_wait_ms`, `checkout_hold_ms`, `compile_ms`, `jev_ms`, and `exec_ms` are reported per call. The per-call dominant-component partition below is descriptive and exclusive; separate component-threshold counts overlap and must not be summed. Concurrent calls can overlap in wall time.
- Also in the fixed host-log prefix: 24 agent-spec preparation records (21 over 10 seconds); 1,837 after-tool slot records (26 over 10 seconds, maximum 39,749ms); 46 compiler-phase records over 10 seconds (5 `compiler_response`, 26 `compiler_transaction_response`, 15 `compiler_transaction_admission`). Admission and response records may overlap, and some admissions have no request ID, so phase rows are not unique compile jobs and are not additive.
- The captured rollout prefixes remain those in the companion report: root 2,450 records/77 Haskell calls; actor 9 2,550/125; actor 10 2,103/98; actor 11 2,319/87; reviewer 13 127/7. `audit-counts.py` fixes those limits and pairs Haskell calls with outputs. It reproduces the corrected rejection categories in the coordination/confusion report using diagnostic-marker precedence (import, parse, scope/name, type/effect), while runtime exceptions are separate. Corrected total: **62 compiler/workbench diagnostic cells plus 2 prepared-runtime exceptions**, not the earlier 65 claim. Exact-input retry remains 1 (root); no same-input retries were found in the other captured prefixes.
- Timing is layered by what the host log observes. `compile_ms` includes waiting/admission and translation/response work; it is not CPU time. `exec_ms` is command/job execution or waiting visible to the host. After-tool elapsed time is nested in the call and includes the hook. Existing Jev cost totals and provider request timing are separate evidence; do not add them to call totals. Exit 137 remains cause-unknown, not evidence of OOM.

## 1. Slow operations and representative traces

### Per-call observed layer

The 458 calls over ten seconds divide by largest recorded internal component as follows:

| Dominant recorded component | Slow calls | Interpretation limit |
|---|---:|---|
| Compile | 257 | Compiler admission/response path; not pure compiler CPU. |
| Checkout hold | 54 | Time holding a checkout during the call; does not by itself establish a blocked actor. |
| Checkout wait | 95 | Queue/wait time acquiring the actor checkout; holder identity is not in this timing row. |
| Command execution/wait (`exec`) | 46 | Workbench-visible command span; some jobs are useful long-running checks. |
| Jev | 6 | Per-call Jev span, which can include the slot path; not the provider-call count in the cost report. |

Component threshold counts overlap by definition: among the 458 slow calls, 83 had checkout wait over ten seconds (max 96,265ms), 31 had checkout hold over ten seconds (max 79,545ms), 187 had compile over ten seconds (max 105,419ms), 47 had exec over ten seconds (max 92,941ms), and 6 had Jev over ten seconds (max 30,091ms). These counts are diagnostics, not a sum of time saved or a partition.

Longest useful examples, all exact host-log line references in the fixed prefix:

- **Compile-heavy tool calls:** actor 11, cell, 111,070ms total / 101,943ms compile at line 84,077; actor 9, cell, 110,644ms / 84,926ms compile at line 83,998; root actor 1, cell, 109,687ms / 105,419ms compile at line 54,992. The compiler phase log also shows actor 20 execution `exec-ed0acdc7588aa73daa1905b54191035c` admitted after a 102,478ms daemon queue span at line 54,767; actor 10 transaction `6ede791438054bb2` returned in 82,942ms at line 124,525; actor 9 transaction `fcc23fab9b6a120c` returned in 75,525ms at line 83,842. These rows are examples of compiler-path latency, not additive spans.
- **Checkout wait:** actor 9 `bash` call at line 113,083 took 98,685ms total, with 96,265ms checkout wait and only 1,092ms exec. Actor 44 `cell` at line 113,308 took 103,492ms, with 75,733ms checkout wait plus 24,689ms compile. Actor 10 `lookup` at line 113,119 had 40,129ms wait and 58,213ms hold. The last case indicates substantial hold/wait, but the row does not identify why the checkout was held.
- **Command span:** actor 55 `bash` at line 104,162 had 92,941ms exec and 94,463ms total; actor 9 `write_stdin` at line 17,235 had 79,521ms exec and 80,749ms total; actor 9 `bash` at line 11,374 had 74,424ms exec (among slow-call rows). A long command span can be a successful test/build or an intentional wait, not a stall.
- **Startup:** actor 14 startup AgentSpec preparation was 78,724ms at line 11,184. Other startup records include actor 36 at 40,592ms and actor 17 at 38,028ms. These are separately observed startup preparation spans, not workbench calls.
- **After-tool/Jev:** actor 9 cell at line 81,840 had a 30,091ms Jev component and 38,721ms total. Actor 44 cell at line 127,959 had 18,777ms Jev and 32,548ms total. The full after-tool slot record maxed at 39,749ms; it is nested work and must not be added again to the associated call. Reuse `jev-cost.md` for cost/request accounting; this section measures operational latency only.

### What this supports

The most frequent dominant cost in the sampled slow calls is the compiler path, followed by checkout wait/hold. Several extreme wall-time rows are clearly checkout waits or intentional command spans, while some compile spans include queue time. It is not justified to turn these into a model-turn penalty, report an aggregate wall-time sum, infer a saturated machine, or claim a causal watchdog benefit. The instrumentation does support showing an owner when a single checkout wait becomes unusually long, with unknown holder and no cancel/restart action.

## 2. Delegation and product delivery

### Tree breadth, useful depth, and critical dependencies

The execution used two bounded findings-only Luna discovery leaves (actors 4/5), then three Luna component owners (actors 9/10/11). The decomposition followed product seams and dependency edges rather than splitting every file:

- **Runtime actor 9** owned local cancellation/integration and delegated the Engine B regression to actor 14 and Store/reopen coverage to actor 15; exact-source review/repair sat with focused reviewer/repair actors, and the root retained final integration. This depth was useful: independent Engine and Store assertions progressed while the owner retained the shared cancellation and review boundary. A concrete red Store test treated read-only `recover_pending` as a transition; root preserved the ownership boundary and directed Engine to classify/persist interruption instead (retained in `docs/wave18-design-followup.md` and `docs/wave18-execution.md`). Review did not convert a red assertion into authority to widen Store production changes.
- **Operator actor 11** split UI projection to actor 16 and browser acceptance to actor 17, keeping integration/docs with the owner. The UI slice was independently reviewed and integrated. Actor 17 created a useful expected-red production-consumer test while the producer was absent, then correctly waited for the producer contract. The Engine module test (actor 14) also demonstrated B delivery while A stayed pending, but deliberately did not prove the staged A-then-B browser journey.
- **Standalone actor 10** initially kept the producer coherent under actor 19 instead of splitting guessed module seams. The critical delivery failure was module-only progress: actor 19 exceeded 30 visible responses without main.rs consumer wiring, and the checkpoint remained unpresented. Root required dirty-work inspection; stopped-worker edits were retained as WIP (`cc92a307…`) before the active repair owner actor 44 was selected. This supports inspecting and transferring dirty work before reassignment. It does not support inferring inactivity from elapsed time or tool count alone. The original c9 acceptance claim was withdrawn before merge when exact review evidence was absent; the later repaired producer has a distinct exact review.
- Reviewer depth was targeted: the late authoritative checkpoint reports repaired producer `a7d742e8d80b759056f73d146576bb3f645b1605` with typed Accepted review 69 by actor 57, verified candidate HEAD, and owner integration `a5409d0d36ed8a3603b77ae8a0e16d1a98d56ab3` reporting seven focused checks 1/1. This contrasts with the earlier prose-only/candidate-text acceptance claim for c9, which root rejected. The exact repaired source review catches and source identity are retained in run `NEXT.md` and `docs/wave18-execution.md`; passing owner checks are not yet the same evidence as root integrated product acceptance.

Useful depth preserved exact ownership/review boundaries and allowed disjoint code/tests to proceed. The run did not establish net speedup: parallel reviews, check collection, cleanup/interviews, and root source/integration decisions all remain visible costs.

### Current product gate at the retained checkpoint

The authoritative status I inspected is the latest committed checkpoint at the top of run checkout `NEXT.md` and `docs/wave18-execution.md` Remaining gates; it is not a fresh live actor poll. At that checkpoint:

1. Root was integrating repaired producer `a7d742e8…` after exact typed review and an owner-reported seven focused 1/1 checks. This is an in-progress integration state, not final root acceptance.
2. Root found native `async: true` tool-definition/call-item metadata missing from scenario definitions/items. `Engine::tools` does not add those fields automatically. Retained request 71 owns the narrow metadata plus a focused production request-boundary test.
3. The whole browser product gate remained open: no integrated async browser green was claimed. Operator request 7 remained pending; actual browser/capture/Store lifecycle acceptance, corrected consumer guidance, root combined checks, and staged-binary journey were still required.

The positive consumer-side expected-red at actor 17 is a useful counterexample to “wait for green before writing tests”; the module-only actor 19 delay is the counterexample to accepting a library/module slice as the first production checkpoint. They justify a milestone defined by the actual production invocation/request boundary, not just a committed module or passing Engine unit test.

### At most two policies worth shadow replay

1. **Coalesced checkout-wait observer:** if one owner call has `checkout_wait_ms > 30,000` (or repeated waits over that threshold for the same work item), record one owner-visible diagnostic with observed wait, call/actor, and “holder unknown.” Do not interrupt, cancel, or reassign. Shadow-replay the fixed host log first. Evidence: 83 slow calls exceed 10 seconds in checkout wait; the longest examples are nearly 96 seconds, and the timing is distinct from compile or command execution. Exclude compile-only spans and long useful command jobs; actor 55’s 92.9-second command is a benign long-execution counterexample, not a checkout-wait stall.
2. **One declared-consumer-checkpoint reminder:** at the owner’s agreed first compiling production-consumer checkpoint, if the submitted candidate is still module-only and lacks the exact `main.rs`/request-boundary evidence, send at most one reminder asking for that artifact/status. Before any reassignment, require explicit dirty-work inspection/transfer. Shadow-replay against actor 19’s module-only episode. Exclude actor 17’s expected-red browser work while the producer is an acknowledged external dependency, and exclude actor 11’s still-pending delivery without a fresh owner observation; do not infer those are stalled from the checkpoint alone.

Both are proposals for replay, not validated causes or established savings. No generic after-tool reminder is supported by this selected evidence; the retained Jev cost report remains the appropriate cost source.

## Evidence and limits

Observed are the exact timing fields, phase/startup/after-tool spans, actor tree and checkpoint status cited above. Inference is limited to which single observation could usefully trigger a shadow reminder. The sampled prefixes do not establish why each checkout was held, that CPU or memory saturation caused compiler latency, that actor reminders caused progress, or how many turns/tokens either policy would save. The 137 outcome remains unknown-cause.
