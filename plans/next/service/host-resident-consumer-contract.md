# Host consumer of exact resident cleanup

Seed e74b3a314698ec4af518d41b9b3fcd47fd01f2d5. The existing InteractiveOwners
row remains the only host lifetime anchor, installed before launch. HostToolControl
is captured from the service and stored there before service spawn. Server tasks,
seal/shutdown futures or owned tasks, and their exact results stay in the row;
completion channels notify rather than carry unique ownership. Workers may hold
slot references but never an owner-map/custody back-reference cycle. No task
result or retained resource may be lost when an awaiting future disappears.

CompletionBoundary is the concrete policy scaffold. AwaitingNativeDecision keeps
/completed usable while HTTP new-work admission is fenced. AbortForShutdown is an
explicit existing host-retirement decision, not inference from HTTP task finish.
Native successful release remains unavailable; do not fabricate that branch.

For a live actor call the same stored HostToolControl::quiesce_and_seal(exact
ActorRef). Store/pin the returned future before polling it; preserve through
bounded timeout without retry. Validate its seal identity. Only after the owner
selects AbortForShutdown may it proceed with shutdown_with_cleanup; retain that
operation/result as well. Preserve completed callbacks until that decision.

For an already terminal actor, quiesce HTTP and inspect its retained terminal()
cleanup directly, without demanding a mailbox seal from a stopped actor. Validate
cleanup.actor() and hook/realm/children components; absent/unconfirmed evidence is
retained uncertainty. Generic ActorTerminal is never cleanup evidence. Confirmed
actor evidence covers only its declared domains, not arbitrary external effects.

HTTP drain is separately controlled: attempt only after the owning completion
boundary is decided and the required exact resident outcome is accounted for.
A conservative unconfirmed path may leave drain pending. Await the original
stored service task; timeout must leave it addressable, no abort-as-complete.
Host cleanup reports actor cleanup, HTTP drain and native/external uncertainty
separately. No scoped launch or binding/build/socket settlement is enabled.

Scope includes actor_host existing owner/launch/retirement/handoff consumer and
private module/tests. Host-tools lead owns host_dynamic_tools and real endpoint
primitive tests; do not edit it. Actor lifecycle semantics belong to retained
actor lead; escalate defects rather than redefining evidence. Socket guards,
launch drain/errors, build exclusive leaf and strict BindingTable generation
ownership must be preserved. Remove superseded dead host staging only where the
actual consumer replaces it; native/external proof holes remain explicit.

Acceptance requires real authored actor + HTTP service through production owner
paths: live exact seal, terminal cleanup path, failed/unsupported/foreign evidence,
completion access until abort boundary, actor shutdown outcome, HTTP drain, lost
seal/service/shutdown waiter and bounded-timeout recovery. No fabricated private
seal/cleanup constructors. Existing addressable drop-before-pin, socket retention
and launch-error seams remain protected. Compile affected production targets and
run focused tests; independent reviewer traces actual production consumers.
