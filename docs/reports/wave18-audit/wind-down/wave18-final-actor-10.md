# Wave18 standalone actor 10 — final interview and handoff

## Scope and exact state

Request 108 was the process-loss recovery continuation, task source
`29b322f079301688d69324d13b146c9ba2286a3c`, with owned paths
`crates/harness-demo/src/main.rs` and `crates/harness-demo/src/async_demo.rs`.
The bounded repair was authored by retained writer actor 44 as
`23f40a48b0678c4df0e457270e938866fd0a58f8` (parent exactly the assigned
source). I preserved that owned main.rs-only commit on this branch by ordinary
cherry-pick; local preservation commit is `a249e2d` (same 5 insertions / 2
deletions). Current branch is
`exomonad/wave18-components/frontier-1/branches/standalone`. No review or
post-repair compile is claimed for either copy. No test, review, or new child
was started after the final supervisor instruction.

## Evidence and blocker

The prior process-loss run artifacts were not available in the writer's bound
checkout; the original run's inner Engine error therefore was not recovered
from those files. Source inspection found startup recovery passing the receiver
from a temporary `watch::channel(false)`. `turn.rs` treats a closed cancellation
watch as cancellation, and the Engine wrapper had hidden the error. The owned
change retains the sender across recovery and includes the Engine error text.
This is a source-backed diagnosis, not recovered original-run observation.

The single authorized post-fix canonical browser run was reported at exact test
source `29b322f079301688d69324d13b146c9ba2286a3c` plus writer candidate
`23f40a48b0678c4df0e457270e938866fd0a58f8`: 1 matched, 1 runnable, 1
executed, 0 passed, 1 failed (exit 101), evidence
`.exomonad/build/cargo/debug/deps/focused-aoweh61r/{evidence.json,output.log}`;
source digest `fa8522be06dcf2215f2729b56bbb299cb3210bc23edf3451f965f16d2bd0dba4`.
Frontend preparation passed (`npm ci`, typecheck, 15/15 frontend tests, build).
The test reached process-loss reopen; the third server failed with
`UNIQUE constraint failed: requests.parent_id, requests.branch`. The test
cleanup removed its reported Store/capture directory, so no DB/capture is
available. Source inspection indicates Engine recovery attempts a child request
under a parent whose unique branch slot is already occupied by the in-flight
continuation. This is a Runtime Engine/Store ownership boundary, not a proven
producer-only cause; no Store schema or API was changed. No retry was made.

No focused regression, production bin compile, exact-source review, or passing
post-fix browser acceptance is evidenced. The next owner is Runtime/Store to
establish a recovery-head/branch invariant that preserves existing request
history and claim recovery; then rerun the canonical process-loss check once on
the repaired shared contract. Operator's browser test was not edited.

## Workflow interview

- The useful automation was the pinned focused-browser runner/preparer: it
  retained matched/executed counts and the failing output, and the one
  post-fix execution located the next boundary rather than inviting a blind
  retry.
- A failed admission attempt passed a function-valued Task to the retained
  implementer because the required owned paths, acceptance, and source
  arguments had not been applied. Request 109 made no edits or checks. The
  correction explicitly bound a complete `Task` at source 29b with the two
  owned paths and acceptance before reassigning the same writer as request
  110. Improvement: expose a constructor/validator that rejects function-valued
  or incomplete Task inputs before request admission and prints the complete
  resolved source/paths/acceptance.
- Earlier in this component, `SessionHelpers.runCheck` was unavailable in this
  actor's lookup; direct `scripts/cargo-focused-test` commands were used. A
  useful helper would bind exact candidate OID, filter and expected match count,
  retaining full evidence and exit status. No such helper was used for this
  final process-loss run.
- The retained browser artifacts and test-cleaned temporary DB were not
  recoverable from this checkout. Do not infer missing output as empty or
  recreate the run. The hosted status reports and source trace are reports and
  inference respectively, not independent execution evidence.
