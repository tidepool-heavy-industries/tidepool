# Application and Shoal host

- `src/shoal.rs` owns CLI/project defaults, tmux startup and environment
  forwarding. `src/actor_host.rs` composes actor runtime, worktrees, provider
  launch and hosted workbench; extend these owners instead of adding launchers.
- Backend protocol/process details belong in `tidepool-agent`; actor identity,
  requests, watches and authority belong in `tidepool-actor`; compilation and
  resident-machine mechanics belong in `tidepool-runtime`.
- Keep `InteractiveGoalPolicy::Disabled` uniform at the shared Shoal launch
  entry point, including root and resumed nodes. Shoal owns continuation;
  role-dependent goal tools would change the first cached provider input item.
- `launch_effort` defaults omitted fork effort to Low, not the root's effort.
  Preserve explicit fork overrides and configured fresh/resumed root effort.
- `src/actor_host/prompt_catalog.rs` freezes one shared superset base composed
  from the run-selected core prose (default `prompts/shoal/base.md`) and
  the shipped `api-guide.md`. TOML workspace overrides freeze at startup;
  never reread them per actor. Do not specialize that prefix
  by role or append dynamic binding inventories. Role and authority observations
  are separate context, not permission granted by inherited text or bindings.
- Fork exact parent context at the specified tool boundary. A published candidate,
  accepted integration, delivered baseline and recipient's checked incorporation
  are distinct facts. Do not infer later stages from earlier receipts.
- Active-update admission is not presentation; presentation is not incorporation.
  Preserve unavailable/unconfirmed outcomes and correlation. Never silently turn
  failed steering into a new queued assignment.
- Reuse Codex's `CODEX_ROLLOUT_TRACE_ROOT` request tracing, already forwarded by
  `pane_environment`, for cache diagnostics. Compare actual normalized provider
  input and routing metadata, not only saved prompt hashes. Keep sensitive trace
  capture opt-in and bounded; do not add a competing logger here.

For launch/prompt changes, use focused owning tests: `actor_host::prompt_catalog`,
`shared_api_guide_example_handles_success_and_unavailable`, and
`fork_effort_defaults_low_and_preserves_explicit_overrides` in the `tidepool`
library. When changing launch configuration or tool contracts, update their
scripted-provider fixtures in the same change, including result envelopes and
the conditions that advance the script. Construct valid fixture setup
from production configuration types/defaults; assert expected wire behavior
independently. Keep configuration and request-shape preflight checks in ordinary
focused tests so drift fails before launching an ignored full-TUI test. Check
that an exact test selection actually ran tests. Keep background probes alive
until explicit test cleanup rather than relying on a sleep duration to outlast
compilation. Compile changed consumers and
follow root verification guidance.
Contributor instructions in this file are not shipped model prompts.
