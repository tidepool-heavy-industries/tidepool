# Wave 18 operator component — actor 11 closeout (incomplete)

This is an evidence and handoff record, **not product acceptance**. Request 7 was
left open at the supervisor's direction. The UI slice and operator walkthrough
were delivered locally; the independent browser lifecycle component did not
reach a green focused run, exact final review, or integrated verification.

## Source and integration record

- Assigned base: `1a345c1e56d769b79062c44b78231382cd6f1a1f`.
- Local operator branch at closeout: `d8af3be10f95486d7247c03d3772c209e4a410d6`.
- UI guidance candidate: `fed2a595edc35a20b482e047ea2b4b4d58a81f4e`; typed exact-scope review request 89 accepted it against the assigned UI scope. Review checks included pinned web check/build and 15/15 tests in five files. The reviewer did not run the browser journey.
- Root integrated UI history includes `007e876cfd983b8c9d9ca94fb14238e057f73bea` and later guidance integration `e1ed92662c5408e4d218b8428ce66f9a5d380c44`; the root reported pinned TypeScript check, 15/15 tests, and Vite build passing at its integrated UI sources.
- The reviewed repeat-safe producer was incorporated at `28cc16cda169a5c2ea5de201740030763729d820`. Native `async:true` metadata was integrated at `3323606b18c04dd10a38b67006fed2a2cfe38a9b`; its focused root gate matched/executed/passed 1/1 and cleanup was clean. Producer source `ec16ee77b365db1cfd883b57102c0d7872e18cbd` was separately reported accepted by review 65. These producer changes are not this actor's owned edits.
- No browser-test candidate below was merged into the operator branch or root integration. No final review of the cumulative browser test plus operator docs was completed.

## Browser test work and exact evidence

Owned acceptance file: `crates/harness-demo/tests/standalone_browser.rs`. The test was extended to drive the actual staged launcher and async lifecycle; call/request identities are observed, not hardcoded. Root identified the correct immutable boundary: `Store::events(None)` model-turn events deserialize as `RecordedReplayTurn`; `model_request` is the exact as-sent input recorded before later outputs mutate Store rows. The production capture writes `transport::client::request_body` unchanged.

- `d1b383e97a03243207e295ce7a618c81955bae7c` (test-only diff from accepted producer `ec16...`) attempted chronology-indexed capture correlation. Focused job `ad264be2-af71-4e0a-8c7a-96740a1f6314`: preparation passed; 1 matched/runnable/executed, 0 passed; B-output assertion observed 0. Evidence references supplied by the worker: `.exomonad/build/cargo/debug/deps/focused-fg2hx1qa/output.log` and `evidence.json`.
- `49635194a8741990f889c38ce072d212b6362b05` used immutable model-turn records and exact serialized request correlation. It passed the B-once/A-unanswered assertions, but the focused run failed later at line 899 because a pre-async decision snapshot was compared with decisions added by the valid async scenario. Attempt 1 on intermediate `ac8d27f4c752a30a3f380ffa49500958cd3ab83c` failed an obsolete mutable-Store-row-to-A-capture assertion at line 830. Final attempt at `4963519...`: 1 matched/runnable/executed, 0 passed. Full log/evidence: `.exomonad/build/cargo/debug/deps/focused-_xu6hiut/output.log` and `evidence.json`.
- `5c0823e90b165d4ac8d87f4fc0f6eee6778d15a2` separated the early hook baseline from a fresh reopen baseline taken after the first process stopped and immediately before the second process launched. Its one authorized focused run was job `32fa3889-4e94-430c-b922-a167343ee63c`: 1 matched/runnable/executed, 0 passed, exit 101, failing in `ready()` at line 83 before reaching the new assertion. Root later recovered the original process evidence and confirmed a genuine panic after 0.67 seconds; no phase/cause was in that original output. No retry was made.
- `29b322f079301688d69324d13b146c9ba2286a3c` added launch diagnostics to every `ready()` callsite: phase, exact binary path, temp DB/capture paths, observed exit status and bounded piped stderr only after observing process exit. The readiness timeout stayed at 10 seconds and `Child` ownership/cleanup were retained. Pinned preparation passed (`tsc`, web 15 tests, build). Its single canonical focused job `688fe0d2-c3ef-478a-9523-ad1ecbd9c58c` matched/runnable/executed 1/1/1, passed 0, exit 101, cleanup clean. It reached `process-loss-reopen` and reported binary `/tmp/exomonad-actor-workspace/.exomonad/build/cargo/debug/harness-demo`, database `/tmp/tidepool-dev-shell.xyK4Oc/nix-shell.zVtKm5/harness-standalone-6146-1790486983672830035/private/session.sqlite`, capture `/tmp/tidepool-dev-shell.xyK4Oc/nix-shell.zVtKm5/harness-standalone-6146-1790486983672830035/outgoing-requests.jsonl`, exit status 1, stderr `harness-demo: deterministic engine request failed`. Log/evidence: `.exomonad/build/cargo/debug/deps/focused-zgfyy63i/output.log` and `evidence.json`. No retry was made.

The exact remaining blocker is **process-loss-reopen's deterministic Engine request failing with exit 1**. Root routed this to Standalone's retained owner, with producer/main/async-demo ownership only. A possible dropped watch-channel cancellation sender and masked inner error were stated as a source suspicion, not as a proven diagnosis. Root instructed this actor to hold browser reruns and test edits pending that producer result. The lifecycle acceptance, final exact review, and integrated browser verification remain open. Do not characterize any of these failures as a passing acceptance check.

The pinned web preparation repeatedly passed at browser candidates (`tsc`, 15 web tests, production build); that is separate from the failing Rust browser journey and proves no async lifecycle behavior. The earlier baseline browser journey at `5484b2a` passed 1/1 but did not cover this async lifecycle. The final staged-release gate `scripts/prepare-browser-harness` was not run by this actor.

## Operator documentation and automation

`docs/wave18-operator.md` was authored/audited for launch paths, readiness versus authentication, session/login, async command semantics, reconnect/reopen, and limitations. Relevant local commits include `4c905593c3486d55b11fd148a119c22756090d30` and `d8af3be10f95486d7247c03d3772c209e4a410d6`. Browser behavior it describes must be read with the evidence above: documentation is not proof of a green run.

Useful automation:

- `scripts/prepare-browser-check <candidate>` consistently automated pinned web setup, TypeScript, Vitest, and build; candidate UI runs reported 15/15 tests. `scripts/cargo-focused-test --package harness-demo --target test:standalone_browser --filter standalone_missing_assets_and_clean_and_process_loss_reopen --expect 1` gave the unique matched/runnable/executed counts and retained log/evidence paths.
- `SessionHelpers.runBrowserCheck` combined preparation and the focused gate in retained job `ad264be2-af71-4e0a-8c7a-96740a1f6314`; `readGate` exposed the final structured status but its completion/read pattern was awkward. On later rounds the exact prescribed `CARGO_BUILD_JOBS=1` command and 6 GiB budget were invoked directly. `runCheck`/`startCheckPlan` were not useful for this exact standalone browser invocation and were not used.
- No `Project.Merge.mergeInto` was used: its advertised red-head rollback conflicts with the standing no-reset/stash/path-checkout policy. UI integration used ordinary Git. No broad process kills or guessed cleanup identities were used.

## Coordination and improvement

Biggest avoidable coordination cost: the UI guidance review was initially requested with the original `1a345c1` basis even though that follow-up's actual assignment base was `7b2f189...`. The reviewer correctly returned a scope repair because the packet compared imported ancestry as if it were UI-authored. The eventual isolated candidate `fed2...` and exact review 89 resolved this without a product change. Improvement: build review packets from the exact assignment base and enumerate the owned cumulative paths before admission; imported accepted ancestry should not be mistaken for changed scope.

A second workflow lesson: the browser capture seam was initially guessed from mutable request-local `Store::items` and chronology, causing avoidable failed assertions. The source-backed `RecordedReplayTurn` model boundary finally gave the correct as-sent evidence. Improvement: inspect the durable API semantics before choosing the test join, and preserve exact capture/Store artifacts with each focused run. Startup diagnostics added in `29b...` now retain launch phase and bounded stderr, which recovered the decisive process-loss error.

The UI reviewer interview said the pinned preparation script was present but initially overlooked when npm was absent; the later exact-candidate review used it and passed web checks. The UI implementer reported that pinned preparation automated setup/check/test/build while manual test failures still required diagnosis (rendered job ID and markup-sensitive matcher). Browser worker interviews reported the focused runner's count/evidence support and manual source-contract work; early helper discovery varied by child environment. A concrete helper improvement is to expose a consistent retained browser-check interface that always returns candidate, prep result, exact matched counts, original job/evidence references, and explicit browser-not-run state, while permitting the required CARGO job/memory settings.

## Next action

Wait for Standalone's scoped producer diagnosis/repair and its reviewed/integrated source. Then, on the integrated source, update the owned browser test only if that evidence warrants it, run the exact focused filter with counted evidence, obtain an exact review covering the cumulative browser test and operator docs (excluding separately accepted producer ancestry), and verify the integrated result. Keep request 7 open until those gates and the staged release acceptance are actually satisfied.
