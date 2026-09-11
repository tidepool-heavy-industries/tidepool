# Interactive Codex applications integration

The applications and resident-sleep implementation is accepted on unified main,
paired with native Codex `d0e5fd48e0`. Do not restart A0–A8, the sleep MVP, or their
historical branch maps. The [main integration record](main-integration.md) records
the source, matched native revision, checks and remaining limits.

Keep ordinary interactive Codex TUIs. The current release uses their embedded
native sessions. Shared-server execution is a [subsequent migration](shared-server.md),
not part of this integration.

## Read progressively

1. [Main integration record](main-integration.md): final source and
   acceptance evidence.
2. [Source inventory](current-state-review.md) and
   [integration checklist](06-integration.md): historical reconciliation evidence.
3. For a concrete failure or future change, only the relevant mechanism contract:
   [binding](01-native-session.md), [delivery](02-delivery.md),
   [process custody](03-process-supervision.md),
   [hosted completion](04-hosted-completion.md), [recovery](05-recovery.md).

Mechanism documents are behavioral contracts, not inventories of missing code.
Historical A-numbers and checkpoints identify provenance only. They are not launch
instructions or branches to recommission. Verify the production consumer before
creating later work; a new campaign starts from unified main with a newly chosen scope.

## Required outcome

- Human input and Shoal steering reach one exact executing native session.
  Immutable input IDs survive admission, uncertain delivery and reconciliation.
- Hosted Haskell results are persisted before dependent context forks become
  eligible. Retry or reconnect cannot repeat evaluation or admit a child twice.
- The normal TUI remains usable when hosted coordination fails. Hosted shell
  tools retain their command owner and resource limits; failure does not enable
  an alternate native process launcher.
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
