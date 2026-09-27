# Wave 18 final handoff — actor 44

## Source and result

- Required test-source base was incorporated by fast-forward: `29b322f079301688d69324d13b146c9ba2286a3c` (parent of the consumer checkpoint).
- Preserved consumer checkpoint: `23f40a48b0678c4df0e457270e938866fd0a58f8`, commit `Keep Engine cancellation open during serve recovery`.
- At that checkpoint the worktree was clean. The cumulative diff from `29b322f` touches only `crates/harness-demo/src/main.rs` (5 insertions, 2 deletions); `async_demo.rs` is unchanged. The patch keeps the recovery `watch` sender alive while Engine recovery runs and includes the underlying Engine error text in the deterministic wrapper error.
- The prior exact-source review at Request 100 / reviewer 65 accepted candidate `ec16ee77b365db1cfd883b57102c0d7872e18cbd` against `e42a3c6104c141230edb79df658294636f75b9e7`. It is historical evidence only, not a review of `23f40a4`.

## Process-loss browser evidence and blocker

The one authorized post-repair canonical filter was run at `23f40a48b0678c4df0e457270e938866fd0a58f8`, after pinned browser asset preparation. Preparation succeeded: `npm ci`, TypeScript check, frontend tests (15/15), and frontend build. The Rust filter selected exactly 1 matched / 1 runnable test and executed 1; result was 0 passed / 1 failed (exit 101).

Retained evidence:

- `.exomonad/build/cargo/debug/deps/focused-aoweh61r/evidence.json`
- `.exomonad/build/cargo/debug/deps/focused-aoweh61r/output.log`
- Evidence source OID: `23f40a48b0678c4df0e457270e938866fd0a58f8`; focused test executable SHA-256: `fa8522be06dcf2215f2729b56bbb299cb3210bc23edf3451f965f16d2bd0dba4`.

The test reached the `process-loss-reopen` startup phase. The third server exited before readiness with:

```text
harness-demo: deterministic engine request failed: UNIQUE constraint failed: requests.parent_id, requests.branch
```

The failure is after the A/B scenario and clean-reopen portions, not a passing process-loss acceptance. Test cleanup removed its printed temporary Store/capture directory (`/tmp/tidepool-dev-shell.xyK4Oc/nix-shell.zVtKm5/harness-standalone-12004-1790488151537953138/`), so the persisted claim state and capture could not be independently queried afterward.

Source inspection shows Engine's inherited-claim path calls `recover_missing_job` before it writes a continuation request. The Store schema enforces `UNIQUE(parent_id, branch)` in `crates/harness/src/store/schema.sql`. The observed duplicate-child insertion is therefore a Runtime Engine/Store recovery-continuation seam. Whether `UnknownCall -> Interrupted` was durably persisted before startup failed is **not verified** from the retained artifacts. No Engine/Store edits were made. The single authorized browser run failed and was not repeated; no further source repair was attempted.

## Checks and limits

- The focused browser target compiled and ran; it failed as reported above.
- Pinned frontend preparation passed, including 15/15 frontend tests.
- A final `cargo check --package harness-demo --bin harness-demo` and an exact-tip review of `23f40a4` were not run.
- Do not report browser acceptance, durable interruption, or exact-tip review as passed.

## Helper use and friction

The compiled workflow guidance in `docs/agent-automation-menu.md` describes `SessionHelpers.runCheck` and `SessionHelpers.runBrowserCheck`, but lookup in this actor returned “no match” for both. I therefore ran the repository commands directly: `bash scripts/prepare-browser-check <candidate-oid>` followed by `CARGO_BUILD_JOBS=1 scripts/cargo-focused-test --package harness-demo --target test:standalone_browser --filter standalone_missing_assets_and_clean_and_process_loss_reopen --expect 1`, with the 6 GiB command limit. They ran in one retained Bash job, preserving the focused runner's evidence files. No test or helper command was rerun after the failure.

## One next improvement

Publish a callable `SessionHelpers.runBrowserCheck` in the actor's bound source, with candidate OID and focused test definition as explicit inputs; return preparation outcome, source/cleanliness, matched/executed counts, and retained evidence paths together. This would make the documented workflow discoverable and keep the exact source and failure evidence attached to the single authorized run.
