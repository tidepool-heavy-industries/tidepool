# HTTP consumer of resident admission seal

Concrete scaffold f5b09855 incorporates approved service68498cf3. Existing
HostToolControl::quiesce_and_seal(expected: ActorRef) -> HostToolSealFuture
immediately changes HTTP admission to Quiescing and returns a boxed owned future.
Polling that future invokes the same service's ResidentToolEndpoint barrier. It
returns HostedWorkSeal only if seal.actor() equals expected exact incarnation;
otherwise HostToolSealError::ForeignActor. Endpoint failure/unsupported is
HostToolSealError::Endpoint. No constructor of resident evidence is exposed here.

Parent must acquire control() before moving service into serve, invoke this before
raw drain(), and retain the future or its addressable task/result in its existing
pending lifecycle owner. Bounded timeout must borrow the same pinned future/task;
dropping it does not establish cancellation, and must not trigger blind resubmission.
Quiesce is monotone, so even unsupported/failed seal leaves new HTTP work fenced.
Completion and registration remain available until separately requested drain().
Seal success does not perform HTTP drain, resident shutdown, process termination,
resource cleanup or custody settlement. Actor lifecycle owner supplies actual seal
semantics; default endpoint explicitly fails instead of fabricating them.

Actual actor barrier implementation is in flight. Tests of unsupported/failure
and retained future cannot close authored-resident integration. Genuine success
and foreign-actor seal checks must use returned private-constructor evidence from
actual resident endpoints, not unsafe constructors or mocks masquerading as owner
proof. No actor/runtime definitions or actor_host edits belong in this branch.

A test fork was rejected before admission because an ancestor had32 active/reserved
descendants. We did not retry admission or widen budget: a new bounded request to
the retained HTTP implementer reuses its context. Existing reviewer is retained
for independent review. This is concrete lifecycle capacity evidence, not a claim
of general fork failure or token savings.
