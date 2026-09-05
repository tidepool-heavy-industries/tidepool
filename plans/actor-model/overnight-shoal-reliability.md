# Shoal reliability execution record

Acceptance scaffold for the approved overnight sequence. Complete all tranches;
use focused checks and one integrated disposable Sol/low live canary.

Current steering: defer the live canary and prioritize the observed orchestration
ownership failures. Keep the prepared checkout for later; do not launch it now.

## Tranches

1. Shared extractor resolution honors Cargo artifact locations and validates the
   matched Haskell worker. Daemon startup explicitly reports direct fallback,
   validates it, observes a real timeout, and reaps owned processes.
2. Provider usage summaries aggregate durable records without polling losses or
   duplicate counts. Exclude inherited history; expose typed scope/completeness
   and preserve first/latest observations. Never claim request completion from
   reply settlement alone.
3. Fresh Sol/low test agents receive no Astra-only configuration updates. Trace
   the installed backend owner; preserve model-specific typed policy. This root
   retains its current history and model.
4. Document compact report projection and independent watches as ordinary
   Haskell. Review exact candidate commits and integrate incrementally.
5. Build the integrated head, launch an isolated shoal-console canary, exercise
   new prompt/pattern errors, unfold/watch/follow-up, provider usage and cleanup.
   Retain evidence; do not integrate disposable canary changes.

## Required evidence

- Focused checks cover target path overrides, startup/fallback failures, timeout,
  inherited ownership, and cleanup.
- Usage checks cover repeated polls, distinct equal-count records, parent
  exclusion, malformed/missing records, history eviction and late observations.
- Sol launch/prompt checks verify effective model/effort and absence of
  unsupported configuration-update injection.
- Compile changed targets, format, diff-check, review consumers. Run fixtures
  checks when translation/serialization changes. Defer live model tests to the
  integrated canary and avoid redundant broad batteries.

## Progress

- Starting head: aca17be5 (pattern diagnostic and basic prompt changes landed).
- Three isolated implementation owners are running with independent typed
  watches: toolchain, usage, and Sol configuration.
- Compact fold example committed in 94769e9f; its report/view declarations and
  pending/unavailable projections compiled and evaluated in the live workbench.
- Disposable console clone prepared at
  `/tmp/shoal-overnight-canary.EhanZs/console`, starting from console commit
  `57e57ffe3e5956174c6c718e57a45fe08bb1a877`. The adjacent `canary-task.md`
  specifies live acceptance and report requirements. Host launch awaits the
  integrated implementation and Sol configuration verification.
- Full acceptance remains pending.
- Sol worker's provider turn failed after invoking native
  `collaboration.send_message` to `/root`; durable rollout contains a native
  self-addressed `agent_message` followed by HTTP 400 rejecting
  `configuration_update` with multi-agent execution. No native subagent spawn
  was recorded. Shoal stop returned `StoppedNow`; root owns its unfinished work.
- Root launch fix disables native `multi_agent` and `multi_agent_v2` for hosted
  agents, keeping actor routing in Shoal. Focused fresh/resume/fork Sol/low
  argument regression passed; integrated live verification remains pending.
- Toolchain candidate 310e74cf integrated as d54b2098. Root reran
  `just test-toolchain-scripts` (11 passed) and the exact focused extractor test
  with all extractor overrides unset (1 passed, 41 excluded). The custom Cargo
  target resolved correctly; a real matched daemon started and was reaped.
- Settled toolchain worker received inherited goal continuations. Root retired
  it with `StoppedNow`. Codex destination-local forks use deferred inheritance,
  not absent inheritance. Child launch policy now disables the existing goals
  feature; root preserves configured goals. All 13 Codex node unit tests passed
  without a live model call; `cargo check -p tidepool --lib` through Nix passed
  for the owning composition root and its dependencies.
- Native PATH Codex is 0.153.4 and cannot read the current fork rollout. The
  running host's executable is
  `/nix/store/ffk5ngbpcrs6y320aabibgrw0zchrajy-codex-rs-0.0.0-dev+118e1cf/bin/codex`.
  Queueing the usage-worker steering through that exact executable succeeded.
- Found the independent Sol control bug: local catalog marks gpt-5.6-sol as
  Responses Lite, while Codex used Lite as the configuration-update gate.
  External Codex patch narrows producer/request projection to Astra. Rust 1.95
  is required (the ambient Tidepool Nix shell supplies 1.93). Two selected
  mocked core tests passed; an added durable-rollout assertion is rerunning.
  External changes are uncommitted until that check passes. No live canary ran.

## Integration evidence

- External Codex fix committed as `702639b55a`. The request-construction unit
  check and mocked Sol/Astra session check passed; the strengthened session
  check also proved zero durable configuration updates for Lite Sol and one
  for Astra. Patched executable build is in progress, without launching a model.
- Usage worker also hit native collaboration failure and was retired via
  `StoppedNow`; no further messages will be sent. Its staged candidate was
  preserved as f8f8a900 and integrated as 2a2b9e3f4 for root review. Root added
  provider-error completeness handling and its regression.
- Fourteen selected usage/parser/runtime/schema tests passed. The generated
  Haskell public-surface test passed with explicit summary-field projections.
  The protocol generator check reports all files current. `just fixtures-check`
  passed all 217 semantic fixtures and the source fingerprint.
- All five workers in this thread are stopped. Current tmux session
  `shoal-tidepool-test` contains only Compiler, Host and actor-0-1. Other
  sessions were not changed. Worktrees and commits remain available.
- Live canary remains deferred at the user's request. The current running
  host keeps its old snapshot; a future launch must use rebuilt binaries.
