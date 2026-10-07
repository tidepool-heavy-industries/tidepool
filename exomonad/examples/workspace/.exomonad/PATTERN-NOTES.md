# Command triage exemplar: authoring notes

Compiled exemplar, not installed in a live workspace. Base shared package: eb73928.

The pattern exposes two question values and a pure decision function. The authored
check and package-fetch examples combine them in a packet and return the original
Jev response beside the proposed follow-up. Neither executes a remedy.

First revision from writing the two clients:

- Initially the transient-failure question ran even for a caller that disallowed
  repetition. The packet now uses existing `J.optional` to omit it in that case.
- A speculative transient answer is interpreted only for the tooling branch;
  uncertainty on that unused question cannot veto an established assertion path.
- The default test criteria did not fit package fetching. The fetch client replaces
  the criteria and explicitly excludes credentials, payment and invalid versions.
  It reuses the questions and planner without copying the transport/decoder.
- Jev judges possible transience; the caller's separate repeat allowance controls
  whether a repeat can even be offered. Returned plans are advice, not commands
  or proof that an action is authorized. Existing command ownership is unchanged.

Remaining ergonomic questions to assess after compilation/live examples:

- Do the explicit packet type synonyms burden most clients, or only exported
  reusable procedures? Keep the local notebook example inference-friendly.
- Should the reference/excerpt pair become an existing retained-evidence value
  at the actual caller, rather than another parallel receipt wrapper here?
- Compare one speculative packet with conditional second-call execution on a
  real workload before choosing a universal batching default.

Focused cases are in Project.CommandTriageChecks. The deterministic construction
case must prove optional omission in the actual prepared request, not merely
successful construction. Live semantic probes and decoder replays are independent of that mechanism check.

Peer review exposed a naming error: source/type errors and bad arguments shared
a branch called CorrectInvocation. That now yields InspectInputs, accurately
covering source, arguments and deterministic setup. RepeatAllowance is explicitly
advisory: the caller must consume its own budget; copying a value does not grant
once-only execution. The diagnostic reference is also a caller-supplied locator,
not a forged claim that a runtime receipt has been verified.

## Second pass from execution and peer review

The repeated-failure branch returns `Settled p Followup`, retaining the caller's
policy in its type. Criteria now include an exhaustive `FailureKind -> Text`
renderer; the fetch client supplies fetch-specific alternatives instead of
inheriting source/type-error language. Shared `triageState` and `triagePacket`
values support both `J.ask` and exact request preparation for inspection.

The initial 12-request cross-pattern trial sent 6,781 input and 686 output tokens.
Its six command cases selected the intended branch for assertion, invalid args,
503 without repeat allowance, 503 with allowance, and exhausted credit. Missing
output yielded policy doubt (confidence 0.36), not an actionable remedy. All six
responses decoded and passed through the Haskell policy. These are synthetic
cases, not a reliability estimate or evidence of frontier turns saved.

The recipe host does not install Jev. Each request was prepared once in Haskell
from its model, state and packet, sent to the live endpoint by the operator, and
the retained prepared value was reused to decode the response through `J.decode`
and the typed planner. This verifies request construction and interpretation,
not the native Jev effect transport. No command was retried by this trial.
