# M1 browser and embedded-host coverage inventory

This inventory maps the M1 contract in
[`engine-harness-completion-wave.md`](engine-harness-completion-wave.md) to
current production owners and available tests. It is a coverage plan, not an
acceptance report. As of 2026-09-30, the actual ignored Playwright journey has been explicitly
executed: its first run failed at an authenticated-status selector. The repair
and rerun are in progress; no passing browser journey is claimed. See the
[checkpoint ledger](engine-harness-completion-evidence.md).

## Contract-to-test map

| M1 behavior | Production owner | Existing evidence and fidelity | Remaining proof |
|---|---|---|---|
| Browser input is admitted once and appears in model history; raw Haskell call/result stays exact | `embedded_service::submit_browser_command` and `drive_conversation_with_transport`; `embedded_harness::EmbeddedConversation` | `actor_host::m1_host_tests::production_host_retains_http_haskell_commands_and_reconnects_without_replay` manually drives the production HTTP/WebSocket host and real resident Haskell (`40 + 2`). It checks exact input/output inclusion, one Haskell call, a duplicate command POST, reconnect history and retirement. It does not execute the browser UI. `production_browser_executes_resident_haskell_retries_and_controls_root` is the real Playwright/real-Haskell route but is ignored. | Execute the ignored Playwright route with the declared browser closure. Add a typed function-call case if M1 requires typed calls through the embedded provider, since the browser scenario currently issues only the raw `haskell` custom call. |
| Raw and typed calls can remain pending across compaction; late outputs are appended once and cancellation cleans claims | Harness `Engine` and the embedded service; facade fixture in `embedded_pending_compaction_tests.rs` | `production_engine_carries_raw_and_typed_pending_calls_through_compaction_and_late_output` runs the real embedded Engine/Store with a gated mock `ResidentToolEndpoint`; it exercises raw and typed held calls, compaction, released late outputs and cancellation. It does not run an actual Haskell cell for those calls or the browser UI. `production_engine_compaction_failure_continues_once_then_cleans_pending_call_on_cancel` covers the failure branch with mock calls. | Add/execute a real resident-Haskell integration scenario if M1 requires these same calls to cross the browser boundary; preserve exact call/output counts and cleanup ownership. Do not cite the mock endpoint test as real Haskell coverage. |
| Browser page reload, status recovery, and explicit same-operation retry; reconnect alone never resubmits | Harness `Operator`, `App`, and `connectHarness`; host `/api/commands/{operation_id}` | UI Vitest tests cover component/protocol behavior, including `sends input, interrupt, and retire to the explicitly selected exact actor`, reconnect projection, and bounded receipts. They mock server/browser I/O. The ignored browser journey has `reload` then `retry`; its socket shim deliberately drops the first ack/receipt and checks exact operation ID, payload, recovered status, and no automatic replay. | Execute the browser journey. Manual host reconnect in `production_host_retains_http_haskell_commands_and_reconnects_without_replay` is useful production-host evidence but is not browser storage/reload proof. |
| Operation UUID binds the exact run/incarnation/action/payload; wrong targets/conflicting retries refuse before effects | `embedded_service::dispatch_embedded_browser_command`, Store retained command owner, harness `Operator` | `embedded_command_tests::production_dispatch_admits_input_only_to_the_exact_actor`, `production_dispatch_refuses_wrong_run_and_incarnation_before_admission`, `production_dispatch_routes_interrupt_and_retire_to_their_exact_actors`, and `admitted_retry_after_retirement_does_not_resolve_admit_or_wake` use fake actor bindings to prove routing/idempotency at the command seam. `App.test.tsx` checks selected-actor targeting; the ignored browser journey captures exact UI command frames. | Run the narrow native command-dispatch filter below and the browser journey. The seam tests are not real actor/Haskell execution. |
| Interrupt the exact active round, resume the driver, then accept later input | `EmbeddedConversation::control`, embedded command dispatch, and Harness `Engine` round owner | `embedded_pending_compaction_tests::browser_interrupt_cancels_one_engine_round_and_driver_accepts_later_input` uses a scripted provider and mock hosted endpoint; it tests exact round control and later input. `m1_host_tests::host_cancellation_stops_a_real_running_haskell_cell` cancels a real 30-second resident Haskell cell through the internal cancellation watch, not the browser interrupt command. The ignored browser journey targets a real running Haskell cell from the UI and continues afterward. | Execute the ignored browser case. A separate integration case should retain the command receipt and prove a stale expected round cannot interrupt the successor if the real browser route does not already exercise that race. |
| Retire root, reject further input, expose terminal lifecycle and clean resources | Host lifecycle and `dispatch_embedded_browser_command`; fixture stop owners | `production_host_retains_http_haskell_commands_and_reconnects_without_replay` manually retires the real root after the Haskell call and checks refused follow-up input. `production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure` checks production-host `lost` projection/refusal after a scripted invalid-auth terminal. `m1_browser_process_tests` covers browser driver process/pipe cleanup (6 helper tests), not whole-host cleanup. | Execute the browser retirement path. Add a true host-loss-during-operation browser integration only if this must prove crash/loss rather than the existing invalid-auth/lost lifecycle. Assert bounded browser, host, forest and hosted-task cleanup with uncertainty surfaced. |

The closest suite entrypoints are `m1_host_tests`, `embedded_pending_compaction_tests`,
`embedded_command_tests`, and `embedded_notification_tests` in
`bridge/facade/src/actor_host.rs`. `embedded_command_tests` includes production
dispatch routing tests over fake actor bindings; `embedded_notification_tests`
exercises the Engine/Store notification owner. Neither is a real-browser
substitute. Harness revision `31ae2e52ca091235c670dbe28aaf730ae148cd4c` has 44
UI tests (Vitest/jsdom) and 19 Rust tests reported for that matched slice; those
component counts are not browser journeys. Earlier 13/19/44 reports and the
six browser-process helper tests must remain separate from M1 acceptance.

## Feature and target boundaries

`m1_host_tests`, `embedded_pending_compaction_tests`, `embedded_command_tests`,
and `embedded_notification_tests` are `#[cfg(test)]` modules and remain
available under `--no-default-features`. This is the required native profile:
`bridge/facade/Cargo.toml` defaults to `codex-compat`, while the embedded M1
path must prove it runs with Codex compatibility disabled. Several unrelated
facade fixtures, including `hosted_retirement`, `hosted_tools_tests`, source
reload and broader actor-host suites, are `#[cfg(all(test, feature =
"codex-compat"))]`; their default-feature passes do not prove the no-Codex M1
path. Conversely, the no-default test configuration excludes those tests and
cannot report them as executed.

The generated Buck unit target is `//bridge/facade:tidepool_unit_tests`; its
browser resources are `//build/testing/browser:driver_bundle`, `//web:dist`,
`toolchains//:browser_test_closure`, and `toolchains//:playwright_browsers`.
The M1 integration is a Rust unit test with a Playwright subprocess, not merely
the Buck web test target. Native Buck execution is still pending; keep Cargo as
the executed-test authority until the project accepts the Buck test runner.

## Focused closure sequence

1. Run the no-Codex real-host tests (Haskell output, reconnect/duplicate, real
   Haskell cancellation, and lost-root rejection):

   ```sh
   bash scripts/dev-shell.sh cargo nextest run -p tidepool --no-default-features --lib -E 'test(actor_host::m1_host_tests::production_host_retains_http_haskell_commands_and_reconnects_without_replay) | test(actor_host::m1_host_tests::host_cancellation_stops_a_real_running_haskell_cell) | test(actor_host::m1_host_tests::production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure)'
   ```

2. Run the no-Codex real Engine control/compaction fixtures. Treat the raw/typed
   compaction cases as mock-hosted-operation coverage, separate from Haskell:

   ```sh
   bash scripts/dev-shell.sh cargo nextest run -p tidepool --no-default-features --lib -E 'test(actor_host::embedded_pending_compaction_tests::production_engine_carries_raw_and_typed_pending_calls_through_compaction_and_late_output) | test(actor_host::embedded_pending_compaction_tests::production_engine_compaction_failure_continues_once_then_cleans_pending_call_on_cancel) | test(actor_host::embedded_pending_compaction_tests::browser_interrupt_cancels_one_engine_round_and_driver_accepts_later_input)'
   ```

   Run exact identity/refusal cases separately; this is host-command seam
   coverage over fake actor bindings:

   ```sh
   bash scripts/dev-shell.sh cargo nextest run -p tidepool --no-default-features --lib -E 'test(actor_host::embedded_command_tests::production_dispatch_admits_input_only_to_the_exact_actor) | test(actor_host::embedded_command_tests::production_dispatch_refuses_wrong_run_and_incarnation_before_admission) | test(actor_host::embedded_command_tests::production_dispatch_routes_interrupt_and_retire_to_their_exact_actors) | test(actor_host::embedded_command_tests::admitted_retry_after_retirement_does_not_resolve_admit_or_wake)'
   ```

3. After Nix/Buck browser inputs and the resident extractor/worker are admitted,
   run the actual ignored UI-to-host-to-Haskell scenario with the generated
   paths exported by the test environment (`TIDEPOOL_BROWSER_NODE`,
   `TIDEPOOL_BROWSER_DRIVER`, `PLAYWRIGHT_BROWSERS_PATH`):

   ```sh
   bash scripts/dev-shell.sh cargo nextest run -p tidepool --no-default-features --lib -E 'test(actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root)' --run-ignored ignored-only
   ```

   This proves the present real journey only: raw Haskell input/result,
   acknowledgement/receipt loss and recovery, reload/no auto-replay, explicit
   identical retry, interrupt of a running Haskell cell, continue, and retire.
   It does not yet prove typed browser calls or pending-call compaction/late
   output with real Haskell; add that real-consumer scenario before claiming
   those M1 requirements complete. Also retain an explicit host-loss/cleanup
   result rather than inferring it from the successful journey's teardown.

Only after these focused runs pass should the owner broaden to the full
no-default facade unit suite and the matched harness UI/Rust slices. Keep the
exact revision, command, executed test count, exit status, and logs with each
result. No M1, M2, or full-engine completion is claimed by this inventory.
