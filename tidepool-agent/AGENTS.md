# Coding-agent backend boundary

This is the only crate that knows coding-agent backends exist. It owns the
provider-neutral headless step seam, the separate long-lived interactive seam,
backend adapters, cycle saga, and model-policy allowlists; worktree allocation,
actor lifecycle, resident tool policy, and effect-handler wiring stay elsewhere.

- Backend-specific types and names stay under `src/backend/<backend>/`.
  `seam.rs` must not expose or wrap backend protocol types.
- `AgentBackend` is a step function: start, surface a tool call, resume, and
  eventually complete. Run-to-completion helpers are combinators over this one
  primitive.
- `InteractiveAgentBackend` owns stock interactive process launch and its
  backend-native push/archive hop. It is not another spelling of the headless
  step seam, and it does not own actor inbox durability or resident tool dispatch.
- Use one shared `SpawnSubstrate` and one movable `CycleSaga` per cycle. Never
  hold the substrate lock across a backend call.
- One backend instance serves one live cycle. Concurrent cycles must not
  interleave resumes through a shared backend.
- Cancellation must reap the exact process instance before settling the saga.
  Fail closed when identity cannot be proven; never signal a bare reused PID.
- Abandonment is idempotent and retain-first. It settles ownership but does not
  delete worktrees or evidence.
- Model selection uses ordered allowlists and fails when none is available;
  never grow a denylist with an implicit fallback.

## Interactive launch and evidence

- `src/backend/codex/node.rs` renders fresh, resumed and exact-context fork
  commands. Preserve the `--destination-local` / `--after-call` boundary;
  do not substitute task-summary reconstruction for inherited context.
- Shoal chooses native goals Disabled for every node, including root, in its
  host launch entry point. Keep the generic backend's Configured policy usable
  by other consumers. Do not reintroduce a root/child feature-surface mismatch.
- The Shoal host resolves omitted fork effort to Low. The backend renders the
  explicit resolved policy; it must not introduce another defaulting owner.
- `src/backend/codex/active_update.rs` owns correlated active presentation;
  `process.rs` owns proxy/process transport and stream lifecycle. Admission,
  confirmed presentation and recipient incorporation are separate boundaries.
  Keep not-submitted and unconfirmed failures distinct; never silently retry
  uncertain delivery as another assignment.
- Native Codex request tracing owns outbound payload evidence. Reuse
  `CODEX_ROLLOUT_TRACE_ROOT` when diagnosing prefix/cache differences rather
  than adding a backend payload log. Saved rollout metadata alone does not prove
  identical final provider input or provider cache reuse.

For command policy changes, run the focused
`goal_policy_is_preserved_for_fresh_resumed_and_forked_launches` test in
`tidepool-agent`; inspect adjacent command tests for the affected mode. For
active-update changes, use the tests in `active_update.rs` and `process.rs` for
correlation, failure and cleanup paths. Compile changed host consumers too.
