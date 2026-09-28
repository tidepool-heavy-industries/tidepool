# Wave20 product baseline and first parallel work

Read-only preparation, 2026-09-27. This records the surviving standalone
harness source and retained wave19 evidence. It is a brief for the next run,
not a wave19 acceptance verdict or a launch record.

## Source pin and carryover

Start reconciliation from the preserved wave19 branch at
`/home/inanna/dev/exomonad-harness-runs/wave19`, commit
`d22c5a525adb104eedece0fb9fce373520c8b7ad`. The checkout has dirty
`NEXT.md`, `docs/agent-automation-trials.md`, and `docs/exomonad-friction.md`;
retain their content and provenance. Do not use standalone `master` at
`9f1ace7` as a substitute: it has diverged (2 commits unique to master, 75
unique to wave19). `d22c5a5` is a recoverable source checkpoint with scaffold
and command/tool-job timing, not an accepted four-component product baseline.
Pin a new integrated OID only after reviewing and incorporating the candidates
below and running the combined product gates.

| Obligation | Surviving candidate and current evidence | Remaining decision/gate |
|---|---|---|
| Same-branch process-loss recovery | `4d722a5` adds explicit `Engine::resume_from_head`, separates RESUME from a new identical input, and has two candidate-pinned focused library passes. Earlier `8a93701` received `Repair` for inferring idempotency from equal content. | Exact-tip review request 74 was pending at stop. Wire explicit resume only to the demo recovery command; rerun the canonical process-loss browser gate. The original occupied-branch gate failed 0/1 on `UNIQUE`. |
| Store-backed history | `223a000` is the history lead's latest branch tip, including an exact route/isolation test; prior component checkpoint `34efea4` needed the root's Store attachment. | Verify ancestry against `6aab0df`, exact-tip review, protected `/api/history` mount and browser deep link on the integrated source. |
| Portable replay | Branches overlap: `cb9a5fe`/`1d04499`, `126abfd`, and latest scaffold branch `1053dd0` touch replay paths. | Select one reconciled implementation, review actual final tip, prove Store export and restriction/provenance fidelity, then run a genuinely fresh-process fixture import. `1d04499` only dropped Store in one process despite its commit wording. |
| Bounded progress and cancellation | Progress lead tip `098299a`; `a520f21` introduced context types and focused tests include bounded flood and cooperative cancellation. | Reconcile provider/turn changes with demo/replay consumers; exact-tip review and combined compile, saturation and first-terminal gates. The `d22c5a5` provider still has an unbounded progress sender. |
| Timeline | Timeline lead tip `2671aa3`, with observation child `51db9ed` and UI child `090b388`; `d22c5a5` contains reviewed command timing preparation and focused tool-job timing. | Prove request/fork spans from Store and a real deterministic async scenario; wire root-owned protocol/App surfaces and run browser acceptance. Fixture-only layout tests do not establish this. |

The retained `NEXT.md` identifies review requests 72 and 74 as pending; the
root interview was unavailable after host recovery failed. Avoid treating a
branch name containing `review` as a returned verdict. The wave19 run log and
compiler log remain under `.exomonad/logs/` in the surviving checkout. The
canonical browser gate and the four-component integrated result have no
recorded pass at `d22c5a5`.

Before new implementation, make a bounded pass over these exact candidates:
read each cumulative owned-path diff and retained review, select one replay
lineage, repair or reject the recovery candidate, and record what is adopted,
deferred or abandoned. The root owns integration and one combined check on the
selected source. A pending wave19 review is an input to this decision, not a
reason to treat an unmerged tip as the baseline. The new wave20 worktrees can
be based on the clean `rsi/wave20` branch at `d22c5a5` during reconciliation;
each dependent implementation rebases on the subsequently pinned shared
contract and adopted product source.

## Shared contract before three Luna implementation trees

The Sol root should freeze an executable contract first: one `CallId` owns a
single asynchronous job; Store request/claim identities are authoritative;
the as-sent model request never includes a later output; cancellation and
reopening retain typed terminal or unknown state; a new identical input is a
new invocation unless explicit RESUME was requested. Agree on the root-owned
Provider/Engine/Store touch points and compile the shared scaffold before
dependent leaves. Each tree returns an exact OID, owned-path diff, counted
checks, review verdict and unresolved integration needs.

1. **Deterministic async custom-cell seam.** Own the standalone
   `crates/harness/src/cell_job.rs` adapter and its deterministic test paths.
   `CellJobProvider` already runs a JSON `{source}` function tool through
   `JobScheduler`, retains `CellOutput`, and yields typed `Cancelled`, but has
   no production resident evaluator or freeform custom-tool presentation.
   Extend the existing Engine/Store path rather than creating another
   scheduler. Prove a pending cell survives three request-boundary envelopes,
   settles once on its original call, persists output and typed final result,
   and releases execution resources after cancel. Reuse
   `crates/harness/tests/adapter_readiness.rs` as the offline starting point;
   its fake cell is the test boundary for this wave. Root owns the standalone
   request-wire decision and integrated stub test. Binding a real Exomonad
   workbench is future supervisor integration work.
2. **Typed job-side agent operations.** Own the Provider/JobScheduler agent
   operation contract in `crates/harness/src/{provider,turn,agents}.rs` and
   focused tests. Today `CallContext` carries unbounded progress and no
   `JobVerbs`; `Provider::call_agent_verb` makes each provider route crate
   verbs, while the demo `TreeProvider` refuses `here`/checkpoint starts.
   Supply a bounded, typed job authority for spawning and sending from a
   running job, with exact origin, cancellation scope and refusal results.
   Preserve the existing Store/Engine admission and output owners. Test a
   job-owned child versus a model-owned child, cancellation, late settlement,
   and bounded progress under barriers rather than sleeps. Root resolves any
   shared `CallContext` migration before other trees compile against it.
3. **Reusable tree driver.** Own extraction of the lifecycle supervisor from
   `crates/harness-demo/src/driver.rs` into the harness library, plus its
   deterministic driver tests. The demo Driver already has Store-backed
   one-task-per-agent supervision, subscribe-before-scan, inbox rescan,
   shutdown joins and restart tests; `tree.rs` remains a demo provider adapter.
   Make the same lifecycle usable by a second caller without a second
   registry, keeping exact agent/head/inbox ownership and explicit failure
   propagation. Demonstrate root plus two children, completion and restart
   after process loss using a deterministic transport. Root owns final demo
   wiring and the integrated browser journey.

Launch scope is one Sol Medium root and three bounded Luna trees after the
shared contract and product baseline are pinned. Use deterministic replay
and stub transport for the first acceptance gates. Credentialed inference and
replacing production Codex remain outside this wave's acceptance claim.
Native Codex goals remain disabled in the Exomonad tree. Rehearse one bounded
authored coordinator checkpoint episode with the compiled shared helper before
using recursive coordination; retain request authority and refusal evidence.
