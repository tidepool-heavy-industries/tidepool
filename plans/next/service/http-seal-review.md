# HTTP seal consumer independent repair checkpoint

Reviewed exact3714153086c1ec5f83f9c610a2066cc79767a733, preserving prior
review7e0ee0a8. Actor seal constructors remain private; no mock successful or
foreign HostedWorkSeal was constructed. Native/actor semantics remain gated.

## Required source repair before acceptance

HostToolControl::quiesce_and_seal unconditionally calls quiesce, which silently
leaves Draining unchanged, then calls the endpoint barrier when polled. Thus
calling it after drain can submit a seal operation with completion HTTP already
unavailable, despite the method's unconditional completion-preservation wording.
The documented caller ordering is not enforced at this owning entry. Add a typed
already-Draining failure, using the same phase synchronization as quiesce so the
admission check/Serving->Quiescing change has one linearization point. A rejected
call must not invoke endpoint.seal_hosted_work_boxed. Test with a counted failing
endpoint (no private seal forgery). Cover Draining both before server starts and
while it exists. Clarify concurrent raw drain after a valid call is a separate
host operation: either prohibit through ownership or document retained caller
responsibility; do not promise unconditionally that completion stays available.

This is a source-owner repair checkpoint, not a request queued back to the parent
who is awaiting review. Source parent owns repair; retained test implementer can
supply follow-up tests after a concrete repaired interface exists.

## Supported findings and limits

Quiescing occurs synchronously before returning the lazy future. The future owns
an Arc to the same endpoint and invokes the barrier once when polled; borrowing
that same future across timeout does not submit again. Dropping and recreating
remains uncertain by documented contract, not an idempotency mechanism. Endpoint
failure is propagated and does not reopen phase or automatically drain HTTP.
Exact seal.actor equality includes incarnation. Success/foreign cases are only
source-inspected until genuine owner-created seal evidence exists.

HostState and HostToolControl duplicate Arc references to the same immutable
endpoint; this is not a second endpoint or authorization channel. Prefer a single
owning field during source repair if that avoids future divergence. It is not a
blocking safety finding at this revision.

Endpoint panic before returning its future or during poll propagates from the
returned future; callers must retain the task's failed outcome as uncertainty,
not retry or treat an absent seal as pre-submit proof. No panic safety/retirement
claim is made by this consumer. Production actor_host does not call this control
yet, so there is no validated host call order, pending-task custody, or real
resident retirement here. Existing listener abort remains outside this candidate.

Independent focused execution at37141530: both existing http_seal tests passed;
full tidepool lib test target compiled, private daemon teardown observed.
Evidence target/http-seal-review/tests.log. These passing tests do not cover the
already-Draining defect. Formatting and diff checks passed. No production edits,
running host replacement, or external Codex steering performed by reviewer.
