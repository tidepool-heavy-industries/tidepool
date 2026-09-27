# Wave18 coordination and evidence-quality audit

Read-only supplement to `turns-helpers.md`; reuses its root/child trace inventory and helper findings. I read `/home/inanna/dev/exomonad-interface/COORDINATION.md` to name overlapping interface parcels. No build, test, source edit or live message was performed.

## Coverage snapshot and limits

The detailed host log was growing. This count snapshot covered root rollout through 2,450 JSONL records (91 event turn IDs, 77 Haskell cells), Runtime actor 9 through 2,550 (44 turn IDs, 125 Haskell cells), Standalone actor 10 through 2,103 (44 turn IDs, 98 Haskell cells), Operator actor 11 through 2,319 (98 turn IDs, 87 Haskell cells), and scaffold reviewer actor 13 through 127 (2 turn IDs, 7 Haskell cells). Rollout and host IDs/paths are enumerated in the companion report. These counts cover those captured prefixes only, not all descendants or their entire subsequent history. “Turn ID” counts are trace IDs, not a counterfactual or independently verified count of completed model rounds.

### Rejected Haskell cells in those captured prefixes

I paired `haskell` custom calls with their outputs and counted explicit GHC workbench `<cell>…error`, missing-module, and resident prepared-engine failure signatures. I separately retained runtime exceptions rather than calling them type errors. The single exact-input retry means a later Haskell call repeated the identical input after a rejection; syntax-adjusted attempts are included in rejection totals but not guessed as retries.

| Trace | Haskell cells | Parse | Scope/name | Type/effect | Import/publication | Prepared runtime exception | Exact-input retry |
|---|---:|---:|---:|---:|---:|---:|---:|
| Root | 77 | 0 | 0 | 0 | 1 | 0 | 1 |
| Runtime actor 9 | 125 | 4 | 5 | 12 | 0 | 2 | 0 |
| Standalone actor 10 | 98 | 4 | 7 | 10 | 0 | 0 | 0 |
| Operator actor 11 | 87 | 7 | 4 | 6 | 0 | 0 | 0 |
| Reviewer actor 13 | 7 | 1 | 0 | 1 | 0 | 0 | 0 |
| **Total** | **394** | **16** | **16** | **29** | **1** | **2** | **1** |

Thus 62 cells had a compiler/workbench rejection and two more failed during prepared-engine runtime execution. The revised count uses only paired Haskell outputs with a GHC/import diagnostic; each cell is assigned once by diagnostic precedence: import, parse, scope/name, then type/effect. The captured-prefix parser `audit-counts.py` reproduces the row boundaries and these marker classes. Earlier draft counts overclassified type/effect cells in Standalone/Operator; this table supersedes them. Root's only exact-input retry was `runBrowserCheck`: root rollout calls at ordinals 40/50, failed import output at 43, `reload_helpers` at 45, successful retry output at 53. All other failing inputs were altered on a later attempt or were not retried exactly in these captured prefixes. These are rejection counts, not a count of unique bugs; one notebook cell can contain multiple intended declarations, and failed local writes often stopped later units. Representative child setup churn is visible in actor 10 ordinals 140–210 (successive parse/effect/scope repairs around `unfold`/task construction) and actor 11 ordinals 41–118 (task/unfold construction); I do not convert those retries into speculative round savings.

## 1. Coordination and handoff chronology

### Same-head question: accepted, presented, acknowledged, then separately verified

Standalone actor 10 asked `async-request-head` in its initial assignment turn `01a0e01e-3bb2-7533-9708-630ec4dffe61` (call `call_3udKbGqllwlFvLIFcBoFbfU0`, ordinal 105; valid question after a local scope correction at ordinal 112). Root sent the answer “same Engine::run and actual prior durable request head” to actor 10 (root call `call_r5JQvaDkIUV5kGgFhuUdqkVR`, ordinal 371). In the next root turn the explicit `pollNotification` returned `Right NotificationPresented` (`call_n0dZwozbpApgAa4y3vN6z2h5`, ordinal 499); the question was still in progress in that same work snapshot. Root then used `updateRequest` on request 6 (`call_pvvyHnbj0OnQWc6nGGcy7viF`, ordinal 506). The first poll returned `UpdateUnconfirmed`—durably queued, not known presented (`call_KSiEqr2wEuTdJ6ISR0yXd73A`, ordinal 524, output 527); a later poll returned `Right UpdatePresented` (`call_wE2fwgiAKab15XeoA8hWZNTT`, ordinal 654). Actor 10 later reported the update presented and same-head choice forwarded to its implementation leaf (child rollout calls at ordinals 1061/1068, turn `01a0e03c-8bb1-75e3-b143-e3057a2e30cb`). The handoff was incorporated into the owner's plan, but it was not itself implementation or test evidence. `docs/wave18-design-followup.md` §3 explicitly keeps the later Engine chronology test and production browser acceptance as separate gates.

This is a positive use of delivery-state checks and a counterexample to collapsing them: first `sendMessage` acceptance was not presentation; presentation did not resolve the still-open question; presented update plus owner forwarding did not establish source incorporation. This directly overlaps interface parcel **B (inspection contract)** in `COORDINATION.md`: passive inspection must not count as explicit watch acknowledgment, and another reader must not consume/suppress the owner's notice. Reuse that state vocabulary in the existing `Project.Routing` question collector; don't add a competing receipt registry.

### Browser trigger correction: original request had already settled

Operator actor 11 escalated the producer ambiguity in its turn `01a0e036-cc80-73e2-8f2d-5dbaf66d8b36` (calls `call_OqxisrA3YzCkh5eE1HrMtFrn`/`call_yFpGYonKxgppqfIHGmryAnbG`, ordinals 822/829). Root's corrected `async start`-alone guidance reached browser worker actor 17 through notification 7 (actor 11 call `call_gaG9OD2oRqkKU7gq7HZXHtnU`, ordinal 861; result says accepted, not yet presented). A later attempt to mutate the older browser request via `updateRequest` returned `Left ReplyAlreadySettled` (`call_Zw4AnrsFiy7jGQhDHoMUCpka`, ordinal 1077). The trace therefore supports “message accepted” and “old request already settled”; it does not show that update changing that settled request. The subsequent browser candidate/check path is the evidence for incorporation, not the rejected update. Where a request has settled, deliver a new scoped request or ordinary message and retain the old receipt as history.

The producer trigger itself was a semantic decision and was correctly escalated. The bounded `WorkflowReminders` trial abstained; root's nudge and later expected-red test advancement do not establish causation (`automation-trials.json` `reminder_trial`).

### Premature acceptance claim, then retraction

Standalone actor 10 submitted a Delivery that treated its `Candidate.checkedCommands` text as exact review acceptance (child call `call_TptaH7yTLq0r2kEHzg5Ue0ap`, ordinal 1476, turn `01a0e079-edf6-7d30-96c9-960eee68defb`). It retracted that claim in a later turn (`call_cRZw0L4cqZODNmNRa5UnqDFH`, ordinal 1499, turn `01a0e07d-db15-7130-934a-5dd15150de21`) after root established that reviewer 56/request 63 remained pending and stale request 65 was not a verdict. Root had not merged the candidate and assigned owned-path repair (`01a0e085-88eb-7513-ab49-c2a57283551b`, ordinal 1566). This is a concrete false promotion in prose, not a valid review with a later quality disagreement. It supports opaque admission of an actual `ReviewRequest`/typed review result with candidate, basis, submitted source and verified checkout identity. A later root correction again refused to treat prose as a receipt and asked the owner to identify/poll the original typed response if it existed (root call `call_K5qYrjZjzeAQV9WM9Ut4dfSP`, ordinal 2412). This audit does not establish a settled verdict from that exchange; it does not use the earlier claim as review evidence.

Positive counterexample: root extracted actual typed `Accepted (reviewed)` evidence from Operator's review response before integrating the UI follow-up (`call_Z65rvkQcy1aq6GGtgxQahgx2`, ordinal 766; exact source and basis in child/root docs). Distinctly, the standalone false claim was rejected before merge. These cases argue for a typed provenance gate, not trusting either a `Candidate` string or any message that happens to contain “Accepted.”

### Routine check receipt produced duplicate presentation

The scaffold path awaited and `collectFocused` the same original job; then the completion watcher repeated the already-read result (`docs/automation-trials.json`, scaffold entry; exact start/collect calls in companion report). No command was resubmitted. This is the same distinction parcel **C (result presentation)** needs: a retained result, its compact display, and an acknowledgment/ownership handoff are different events. Let C own payload display/truncation and exact-once render semantics; let `Project.CheckResults`/existing subscriber retain who already received terminal evidence. A second rendering is not a second test.

A second redundant-notice observation is root's `withCheckpoints` router waking for raw candidates before review. Root replaced the sink while retaining the response handles. Interface parcel B's explicit acknowledgment should remain separate from this work-progress notice policy; add review-stage filtering to the existing collector rather than treating a foreign status read as notice acknowledgment.

The Operator worker also reported a repeated “respond now” reminder while request 7 was explicitly held for the absent producer/integration gate. Its exact origin remains unknown in the wave18 coordination notes. Do not attribute it to Watchdog or classify it as an observed repeated generic-hook intervention. See `docs/wave18-rsi-coordination.md` and actor 11's observation turn `01a0e06f-1a6c-7351-ae6d-d686cd093f77` (calls `call_vme7IeS6uMhMUVxXmeti5WqS`/`call_Fnd7U2YWGZbS0CnWm6ff4cKl`, ordinals 2164/2171).

### Positive accepted handoff; incomplete incorporation evidence

Runtime root and owner explicitly kept separate source/check/review/integration states: independent Engine candidate `ea99096…` had an exact reviewed owned path; `preflightSlice` retained a conflict-free prospective tree; root then ran integrated check at `8e44e9a…`, 1/1; later turn cancellation slice was checked at the integrated source. `docs/wave18-execution.md` “Remaining gates” and companion report's preflight trace references preserve this chronology. The module Engine test proved B output in request 3 while A was unanswered for that fixture; the note explicitly says this does not prove the production staged A-then-B browser sequence. This is sound evidence separation and a positive case for only escalating unresolved product semantics.

## 2. Confusion and evidence quality

### Exact source and helper identity drift

- **Disk source vs callable helper:** root first imported `SessionHelpers` from disk and got `Could not find module`; `reload_helpers` published `ec811751…` over helper source `50b4f8a…`, after which the exact same check cell ran. The receipt is publication/binding evidence for that helper version, not evidence that every already-forked child changed revision.
- **Prepared vs active review helper source:** one `ReviewFlow` attempt stopped before admitting a reviewer because prepared helper draft and active revision disagreed (`automation-trials.json` `review_flow`: zero reviewers, zero repairs). It did not expose the revision pair and root used ordinary exact-source review fallback. This is a source-drift failure, not evidence the candidate was wrong.
- **AgentSpec publication vs installed surface:** root's authorized after-tool pruning published layer `7a3d3e…` over `fc61cd…`, but `reload_agent_spec` failed with missing imported `Project.Shell.presentSelected` (root rollout `call_iFOZDv0C6qycpUAuczNdNoe4`, ordinal 2131). Status retained the prior installed tool record; no child reload was requested or claimed (`wave18-rsi-coordination.md`, “Authorized blanket-hook removal”). Do not infer that source publication changed the running agents.
- **Candidate OID typo:** Operator first relayed a nonexistent full UI OID ending `ed6d94`; root resolved the short prefix to `38b84d874845bb7ef614dde0c44e814726ed3d94` before review/integration (`docs/exomonad-friction.md`, Wave18). Actual root review used the exact Git OID. This is positive recovery, but manual copy/repair is avoidable with candidate handles.

Recommended owner boundary: source publication records the immutable source revision and bound/import result; review admission records its active prepared revision and exact candidate. The existing source-layer and `Project.ReviewFlow` owners should expose that pair on refusal. No new resolver. This overlaps interface parcel **E (lookup examples)**: exact resolved declaration identity should drive inline examples; don't make an example imply a symbol is active/callable when source publication/binding says otherwise. Keep ambiguous or shadowed lookup as no-example/explicit-unavailable, and make helper examples point to the published revision/import that actually resolves.

### Rejected Haskell cells: classes, not a success-rate claim

The captured prefixes contain many small workbench rejections while assembling `unfold`, actor messages, tasks and interview values (table above). Counts show categories, not severity. A type/scope error in a Haskell orchestration cell is separate from the two Runtime actor prepared-engine execution failures, and both differ from Cargo/test outcomes. Root's root-only helper-import retry is the one exact same-cell retry; other corrected constructions changed syntax/types, so retry grouping would need a semantic similarity policy not present in the trace. Do not sum these into “failed model rounds” or infer the cost of a fixed Haskell DSL from them.

Common friction visible in the examples: term-level constructors used as values, `Task`/`ResponseResult` shape confusion, multi-line layout, and effect inference requiring an annotation. The repository's `exomonad-workbench` skill already addresses syntax/type failures; this task is read-only and does not propose moving these mistakes into product APIs. Better compiler error locality or a bounded example from the compiled superset guide may help; trace counts do not show which fix would reduce them.

### Evidence statements and failed checks kept distinct

- Reviewer actor 13's exact test runner exited 137 before counts; serial direct Cargo then produced 1/1. Cause is unknown, not proof of memory pressure. A helper fallback preserves both original outcomes (`docs/interviews.md` scaffold interview; `wave18-design-followup.md` §4).
- A typed `Accepted` verdict, when actually present, still records the review scope/source and is not equivalent to an integrated consumer check. In this wave18 episode the actor's early prose claim was not a typed verdict, so it could not satisfy the review gate. Keep review response, consumer audit and integration check as independent evidence sources.
- Root's successful `preflightSlice` means ancestry/scope/merge-tree were clear at a captured HEAD; it says nothing about semantic correctness and can stale. The standalone counterexample forced a scoped repair despite green mechanical preflight.
- Root's Engine concurrency test was 1/1 at an integrated source, but remains narrower than the staged browser A/B contract. A module test or expected-red consumer test does not establish the combined production journey.

### Interface parcel overlap: use shared boundaries

| Interface parcel in `COORDINATION.md` | Wave18 evidence that overlaps | Recommendation boundary |
|---|---|---|
| **B — inspection contract** | `sendMessage` accepted vs `NotificationPresented`; `updateRequest` queued vs `UpdatePresented`; owner acknowledgment vs implementation; repeated `Cmd.await`/collect/watcher reads | Use the same passive-read vs explicit acknowledgment distinction. Do not let inspection consume an owner's notification. Question revisions, message receipts and incorporation evidence belong in existing Exomonad routing. |
| **D — waits and cancellation** (depends on reviewed B) | Runtime cancellation slice canceled a job then forced a late `Completed` settlement through the real settle function; returned and retained state stayed Cancelled (`docs/wave18-execution.md`). Async question/update also stayed pending across an active turn. | D owns user-facing wait/cancel and late-completion behavior in its interface path. Do not duplicate the Rust cancellation scheduler in Haskell; preserve the original outcome and route terminal evidence once. |
| **C — result presentation** | await+collect+watcher duplicated display of one retained test result; some evidence readers failed while original job remained recoverable | C owns actual production payload shaping and truncation. Present counts/source/cleanup/status with recoverable original reference; a page/second read is not a new completion. Existing check subscriber should track delivery/ack once. |
| **E — lookup examples** (depends on reviewed A) | Disk helper unavailable until `reload_helpers`; prepared/active helper revision mismatch; exact import/source needed for review packet | E owns bounded inline examples and exact declaration identity. Show the actual published, callable helper with import/source receipt; leave ambiguous or unavailable bindings explicit. Avoid another lookup registry. |

These are overlaps, not a claim that the interface branch implements Exomonad Work routing or that wave18 validated the new parcels. Parcel B/D/C/E remain separate worktrees and owners under `COORDINATION.md`; review and integration stay with that root.

## Conclusions and limits

Observed: receipts, presentations, owner forwarding, source publication, review results and source/check evidence differed in real episodes; at least one prose “Accepted” was retracted before integration; two duplicate displays/read notices did not rerun work; one source-helper import failure recovered after publication; one live AgentSpec source publication did not install. Exact counts and references are above.

Inference only: a typed, existing-owner continuation could remove manual state reconstruction and deduplicate routine presentation while preserving uncertain/mismatched states. The selected trace inventory has no controlled comparison; it cannot establish saved model rounds, token cost, latency, or causal benefit from a reminder. The interface parcel overlap suggests shared state vocabulary and presentation boundaries, not a universal notification framework.
