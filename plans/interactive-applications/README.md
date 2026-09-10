# Finish the interactive Codex applications integration

Deliver one reviewed Tidepool/Codex pair on the current main foundation. Consolidate
retained applications work, repair remaining behavior, and close its acceptance
checks. This is a wrap-up assignment, not a fresh implementation of A0–A8.

Keep ordinary interactive Codex TUIs. The current release uses their embedded
native sessions. Shared-server execution is a [subsequent migration](shared-server.md),
not a prerequisite or an additional implementation lane in this wave.

## Read progressively

1. [Source inventory](current-state-review.md): accepted foundation, retained
   candidates, evidence limits and source-selection rules.
2. [Integration checklist](06-integration.md): finite remaining work and release.
3. Only the mechanism relevant to the assignment:
   [binding](01-native-session.md), [delivery](02-delivery.md),
   [process custody](03-process-supervision.md),
   [hosted completion](04-hosted-completion.md), [recovery](05-recovery.md).

Mechanism documents are behavioral contracts, not inventories of missing code.
Historical A-numbers identify provenance only. Verify the production consumer
before creating an implementation. The launch record selects exact source and
runtime/package revisions; a historical plan hash never selects the runner.

## Required outcome

- Human input and Shoal steering reach one exact executing native session.
  Immutable input IDs survive admission, uncertain delivery and reconciliation.
- Hosted Haskell results are persisted before dependent context forks become
  eligible. Retry or reconnect cannot repeat evaluation or admit a child twice.
- Native tools and the normal TUI remain usable when hosted coordination fails.
- Retirement accounts for native processes, accepted hosted work, command jobs
  and worktree/build resources through their existing owners.
- Recovery distinguishes reconnection, new execution from retained history, and
  source reconstruction. Git/history does not restore live Haskell capabilities.
- The accepted implementation preserves main's command limits, workspace forks,
  actor routing and prompt package. It is tested using actual binaries and a
  scripted provider, without paid inference or workspace-wide test batteries.

## Ownership boundaries to preserve

Shoal owns actor continuation and typed requests. Native Codex owns execution,
input admission and history. The TUI owns the human interaction; rendering must
not own completion correctness. Existing process supervisors own exact OS scopes;
existing deployment rows retain resource custody. Extend these owners, rather
than introducing parallel registries or controllers.

The current embedded topology need not become a permanent assumption in neutral
interfaces. Keep session identity distinct from pane/process identity, and keep
workspace execution environment explicit. Make only changes needed by current
consumers; do not build a generic backend framework for the later migration.
