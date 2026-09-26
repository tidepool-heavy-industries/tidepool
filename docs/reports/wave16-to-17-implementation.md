# Wave16 → 17 implementation

2026-09-26. Source evidence: `wave16-initial-audit.md` and
`wave16-helper-interview.md`. No successor run is launched by this batch.

## Resident fork publication

Wave16's ReviewFlow reviewer reached queue readiness, but the native host waits
for committed admission before installing its input producer. A resident record
actor has no notebook completion callback to publish that admission. Its handler
could therefore wait for a child that could never receive its first turn.

`ResidentKernelBehavior` now distinguishes resident execution from workbench
execution, including raw operator notebooks without provider provenance. A
resident Commit publishes only its own ready group before resuming the handler.
Notebook groups retain their input/tool-completion publication boundary. Registry
logs now identify commit requested, child queue ready and group published.

The host regression uses a compiled workspace module, a record actor, managed
checkout admission, child activation and a typed reply routed back to the actor.
With publication removed, the gate stayed Ready and timed out; with it installed,
the complete scenario passed. The fixture uses static module request metadata,
as Project.ReviewFlow does. An earlier notebook-declared fixture reached the
fixed publication boundary but lacked GHC request-site metadata; it was replaced.

A separate existing multi-unfold expectation was stale: its second fixture
explicitly requests Low effort, while the assertion expected an omitted value.
The assertion now follows the authored fixture.

## Helpers and prompt changes

Shared workspace `2599a6434b5f09ab176b29a851b6a680ef4aa45d`:

- `CheckDefinition` carries reusable check selection; `runCheck` takes the exact
  candidate at invocation. Effects remain polymorphic, with the notebook result
  bound using `<-`. A concrete CodingEffects annotation is wrong for a root's
  different effect row; the recipe verifies invocation without that annotation.
- `startFocusedAfter` and `startPreparedGate` execute prerequisite argv and the
  focused runner in one original command job. A failed prerequisite prevents
  test execution. Preparation evidence is distinct from test/source evidence.
- Completion actors observe the original job. They acquire no write authority
  to its checkout. Missing/interrupted preparation cannot produce acceptance.
- The harness's `runBrowserCheck` uses that composition. Its preparation wrapper
  checks the candidate before/after preparation, snapshots only flake inputs for
  Nix, reuses `verify-browser-journey --prepare-only`, and checks local assets.
- Role prompts expose the callable session helpers, terminal notices, source
  assurance and format-before-candidate order. Core catalog v41 warns against
  explicit short yields followed by empty polling. Trial guidance records
  useful non-use and preserves the minimum three-exposed-wave window.

The browser preparation and focused test use existing command machinery; this
batch introduces no scheduler, artifact registry, or semantic classifier.
Deterministic prerequisite failure does not require Jev.

## Verification

- Regression baseline: resident admission remained Ready without publication.
- Fixed host regression: 1 passed, including activation and typed reply.
- Fork registry: 13 passed, including readiness, exact boundaries and aborts.
- Focused helper recipes: preparedCompletion 4; completionRouting 13;
  managedEvidence 8. Includes inference, prerequisite failure/interruption,
  checkout authority, retained artifacts and unknown evidence.
- Harness preparation wrapper: 4 Python tests passed; shell syntax checks pass.
- Notebook multi-unfold ordering and catalog fingerprint: 2 passed.
- Workspace pin consistency: 1 passed on committed gitlink/scaffold revision.
- `just exomonad-build` succeeded with the matched local extractor/worker/binary.
- Changed shared sources match the tested snapshot and template byte-for-byte;
  shell syntax and `git diff --check` passed.

## Revisions and retained logs

- Tidepool runtime: `54247c9a9`.
- Tidepool workspace/template pin and catalog: `8c0c77865`.
- Shared workspace: `2599a6434b5f09ab176b29a851b6a680ef4aa45d`, published as
  `wave17/prepared-checks` on its origin; both repository pins use this commit.
- Harness helper/prompts/preparation: `1b0f7a0`.
- Focused logs: `/tmp/wave17-publication-baseline.log`,
  `/tmp/wave17-publication-final.log`, `/tmp/wave17-lineage.log`,
  `/tmp/wave17-notebook.log`, `/tmp/wave17-helper-final2.log`,
  `/tmp/wave17-pin.log`, `/tmp/wave17-build.log`.

The matched binary was built after runtime/pin changes. No shared daemon was
restarted. Successful private check daemons exited through their owning scripts.

These are offline host/recipe tests, not a live provider ReviewFlow success or
measured model-turn savings. Before the next live trial, observe a native first
turn, typed verdict and cleanup on the built revision. Keep the trial bounded.

## Retained follow-ups

- Cancelled Wave16 reviewer: process stop is observed, but retained socket and
  hosted-resource cleanup remain unconfirmed. Evidence and resources preserved.
- The 63-second cleanup cell was dominated by cold compiler reconstruction after
  RSS rotation. No new cache or rotation change is justified by this incident.
- Expanding FailureEvidence with a ninth constructor exposed unsupported
  `dataToTagLarge#` during derived Eq compilation. PreparationEvidence now owns
  prerequisite state separately; a focused compiler capability fix remains a
  separate task. This is an implementation-time observation; the initial failure
  log was overwritten by the successful retry, so a new minimal reproduction
  is required before changing the compiler.
