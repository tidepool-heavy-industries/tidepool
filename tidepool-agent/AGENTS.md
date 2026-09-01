# Coding-agent backend boundary

This is the only crate that knows coding-agent backends exist. It owns the
provider-neutral headless step seam, the separate long-lived interactive seam,
backend adapters, cycle saga, and model-policy allowlists; worktree allocation,
actor lifecycle, MCP policy, and effect-handler wiring stay elsewhere.

- Backend-specific types and names stay under `src/backend/<backend>/`.
  `seam.rs` must not expose or wrap backend protocol types.
- `AgentBackend` is a step function: start, surface a tool call, resume, and
  eventually complete. Run-to-completion helpers are combinators over this one
  primitive.
- `InteractiveAgentBackend` owns stock interactive process launch and its
  backend-native push/archive hop. It is not another spelling of the headless
  step seam, and it does not own actor inbox durability or MCP dispatch.
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
