# Wave20 readiness

Updated 2026-09-27 at the authorized one-shot supervision check.
User authorizes launch once the inter-wave work is complete. No launch has
occurred. The timer is one-shot, not permission to skip gates or recur.

## Latest integration check

Main `44f418d98` integrates storage retirement/recovery, checkpoint capture and
release, fallible record sends, and durable run paths. The combined facade
target compiled; six selected tests passed (535 skipped, 52.088 seconds): four
durable-path/discovery checks, the hosted deferred-checkpoint scenario, and the
fallible mailbox scenario. This supersedes the integration status below.

Post-integration review found checkpoint release removes sponsor metadata used
to charge already-admitted descendants. The checkpoint owner is repairing the
budget ledger independently of capability lifetime. This remains a launch gate
despite the six passing tests.

The coordinator owner now has the compiler slot for authored helper checks.
The source owner is removing automatic checkout-tooling shadowing: ordinary
product work uses coherent run tooling and branch-local notebook helpers.
Explicit per-actor checkout-tooling selection is a documented follow-up rather
than an implicit incomplete module overlay.

## Current evidence

- Wave19 root and worker tmux panes are dead; the compiler pane remains alive.
  Do not restart that shared daemon or confuse retained tmux windows with a
  live frontier wave. The root interview is unavailable after the crash.
- Wave19 root checkout remains at `d22c5a5`, with dirty NEXT, automation trial
  notes and friction notes preserved. Inspect surviving candidate commits and
  their reviews before choosing the wave20 product baseline.
- Source import/admission changes are integrated on main at `6360d2329`,
  `476f7228e`, `4c46712e8`. Root executed both lock tests through `just test-lib
  exomonad-node`: concurrent admission and process-death release, each 1 passed.
- Retirement candidate `dc9707f2c` has focused branch checks, but root review
  found finalized manifests never released their layer references. Repair and
  last-reference cleanup evidence are required before integration. The manual
  16-manifest test is not a production multi-child retirement measurement.
- Checkpoint candidate `c32580a47` has generated checks, five-crate compilation
  and focused tests. Root review found published capabilities retain their
  issuing machine indefinitely; explicit release and a hosted end-to-end test
  remain. Do not claim registry tests prove the provider round trip.
- Coordinator draft `dc7d2f8` in shared workspace branch
  `rsi/w19-continuation` is uncompiled and is not a launchable helper pin.

## Remaining launch gates and owners

1. Storage: complete bounded import/copy admission, durable retired custody,
   reclamation, state/cache separation and failure-path review. Root owns exact
   integration; source/durable-path and retirement lanes own repairs.
2. Runtime: fallible mailbox admission (`R.trySend`), checkpoint release and
   delayed hosted fork test. Checkpoint lane owns implementation. No synchronous
   request disguised as a cast.
3. Authored coordinator: resume the parked typed interpreter after runtime APIs
   settle; compile and execute sequence/parallel joins, exact review completion,
   refused callback retention, explicit corrections and cleanup. Update prompts
   with compiled examples and pin the resulting shared workspace.
4. Source coherence: distinguish historical product source from coherent run
   tooling. Checkpoint-specific frozen source admission alone does not resolve
   the general wave19 historical-review module mixing defect.
5. Product baseline/brief: reconcile surviving wave19 work. Intended wave20
   parallel components are deterministic asynchronous custom-cell integration,
   typed job-side agent operations, and a reusable tree-driver lifecycle.
   Preserve standalone extension seams; no credentialed inference or production
   Codex replacement. Shared contracts precede dependent implementation forks.
6. Build the matched local runtime and execute focused integration/launch checks.
   Record binary/source revision, shared workspace pin, product baseline,
   prompt revision, run/log identity and experiment hypotheses. Launch Sol
   Medium only when the brief, helper rehearsal and storage gates are ready.

Use one expensive compiler slot. Do not apply prerequisite commits twice:
the retirement branch contains cherry-picks of the source/admission lane.
Retain exact command/count evidence and report unavailable behavior honestly.
