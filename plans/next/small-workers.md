# Small typed workers: optional read-only application

This is a later application of the [current implementation plan](../../NEXT.md),
not a service-migration gate or a task tree for the builder. Use the existing
interactive Codex TUI path and the shared Haskell worker primitives.

## Intended outcome

A lead supplies a typed numeric discrepancy to a small read-only Sol worker.
The worker classifies or minimizes it and returns a typed result; the owning
consumer independently checks that result. Reuse existing numeric fixtures and
deterministic computations. This application is not a required model comparison
or evaluation campaign.

The worker receives selected context, a useful Haskell vocabulary, an expected
result, and actual read-only authority. This complements exact-context
specialists. Use Sol Low for routine bounded work and explicit effort/model
selection when the obligation requires it.

## Contracts

- Reuse actor admission, requests, watches, and live-value ownership. No second
  worker manager, mailbox, or provider loop.
- Shared worktree access is explicitly read-only here. Captured functions or
  inherited text do not grant the parent's authority.
- Preserve separate actor/request identities, usage attribution, and reply
  obligations even when a worker is presented in the same pane.
- Load project modules and prompts from the swarm's frozen TOML-selected inputs.
  Local task functions compose over that interface; source changes take effect
  at the next explicit swarm boundary.
- Parent/child calls must not deadlock through an occupied admitting tool block
  or a non-reentrant workbench. Failures remain explicit; do not fabricate results.
- Keep the common tool/prompt vocabulary stable across the task family. Supply
  varying typed input through context rather than rebuilding tool schemas per case.

## Implementation and acceptance

The single implementing Astra uses existing owners in `tidepool-actor`,
`tidepool-agent`, `tidepool-runtime`, and the host/compiler boundaries. Read the
nearest contributor guidance and actual consumers. No independent service
controller or observer is required.

Verify a real bounded read-only consumer, authority rejection, cancellation,
partial failure, response settlement, definition capture, and attribution with
focused owning checks. Compile the reusable specification and its consumer.
Retain exact checked source and remaining limits. Broader write access is a
separate product change, not a prerequisite for this example.

The [small-agent reference](../small-agents.md) supplies the broader interface
requirements; [workspace customization](workspace-pilot.md) owns configuration
and context packaging.
