# Wave18 actor 66 closeout — incomplete

## Exact source and test evidence

- Final owned test candidate: `29b322f079301688d69324d13b146c9ba2286a3c`, based on test candidate `5c0823e90b165d4ac8d87f4fc0f6eee6778d15a2`. Producer ancestry was the explicitly unreviewed `ec16ee77b365db1cfd883b57102c0d7872e18cbd`; this actor made no producer or Store changes and did not approve that preview.
- Current cumulative owned work covers `crates/harness-demo/tests/standalone_browser.rs`; this closeout additionally changes only this authorized report path.
- The immutable boundary repair reads ordered `Store::events(None)` `model_turn` payloads as `RecordedReplayTurn`, correlates the request's `model_request` through production `request_body`, and requires a unique capture match. The B-consuming request had B once, retained original A call provenance, and had no A output; the A-running/delivered-false assertion remained.
- Reopen-baseline repair retains the early decisions snapshot for original hook assertions and takes a separate snapshot after successful `stop_sigint(first)`, immediately before the clean-reopen launch.
- Startup diagnostics now include phase, configured binary, Store/capture paths, observed exit status, and bounded stderr read only after `try_wait` sees process exit. The 10-second readiness timeout was not changed.
- Final prescribed focused command matched 1, runnable 1, executed 1, passed 0; exit 101, cleanup terminal/clean. It failed in `process-loss-reopen`: binary `/tmp/exomonad-actor-workspace/.exomonad/build/cargo/debug/harness-demo`, Store `/tmp/tidepool-dev-shell.xyK4Oc/nix-shell.zVtKm5/harness-standalone-6146-1790486983672830035/private/session.sqlite`, capture `/tmp/tidepool-dev-shell.xyK4Oc/nix-shell.zVtKm5/harness-standalone-6146-1790486983672830035/outgoing-requests.jsonl`, exit status 1, stderr `harness-demo: deterministic engine request failed`.
- Full test output: `.exomonad/build/cargo/debug/deps/focused-zgfyy63i/output.log`; structured evidence: `.exomonad/build/cargo/debug/deps/focused-zgfyy63i/evidence.json`. Pinned preparation passed (TypeScript check, 15 web tests, production build). No retry, repair, or producer diagnosis was performed after the single assigned run.

## Helper use and avoidance

Earlier, `SessionHelpers.runBrowserCheck` was available and used for the preceding capture-correlation work; `readGate` returned the terminal structured result. For the final exact runs here, I used `bash scripts/prepare-browser-check <candidate>` and the mandated direct `CARGO_BUILD_JOBS=1 python3 scripts/cargo-focused-test ...` invocation with 6 GiB. I avoided `runBrowserCheck` because the assignment required that exact canonical shell command and explicit build-job/memory settings. The direct runner retained counts and full log/evidence references.

## Coordination reflection

- Largest wasted step: treating chronological before-request/capture ordering as a request identity join. The failed B assertion showed that ordering alone did not associate the consumer with its as-sent input. The immutable `model_turn` event was the sound boundary, but later attempts then encountered unrelated A-capture and reopen-baseline failures before the final precise process-loss error.
- One next improvement: require source-backed immutable correlation evidence before implementation admission, and keep each prescribed focused run attached to an exact candidate with a retained terminal gate/evidence path; stop at the first explicitly bounded failure instead of carrying assumptions forward.
- Waits/confusion: the retained `runBrowserCheck` gate took several minutes to settle, and its completion actor did not surface a notice in the expected way; `readGate` on the retained handle recovered the terminal counts and evidence. Runner summaries sometimes presented count vectors separately from prose, so I checked both the structured evidence and output before interpreting `executed 0 passed` versus a test failure. Source updates changed the required base; preserving commits and rebasing was necessary to avoid testing a stale producer preview.
- Repetitive sequence to automate: a candidate-pinned browser gate that runs preparation, then the exact focused runner with explicit Cargo job/memory settings, and returns one structured terminal record containing candidate/base, prep result, matched/runnable/executed/passed counts, phase-tagged stderr, cleanup, and full evidence paths.

## Remaining blocker

Wave18 delivery is incomplete. The process-loss-reopen engine failure needs owner diagnosis and any producer change must receive its own review and integration. The focused lifecycle test must pass on the root-reviewed integrated source; this test-only candidate is not final acceptance.
