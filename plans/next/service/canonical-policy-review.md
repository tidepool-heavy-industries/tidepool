# Canonical interactive policy projection review

Accept narrow candidate d393cbb8d406845fc4686147aaa456e2be613ea1 relative to
parent-accepted2db8f2c6. Independent source review only; parent owns the running
external authored integration check and must report its result separately.

The production install_interactive_policy already resolves the exact actor from
its session context and constructs ResidentInteractivePolicy::local. This change
exposes that same constructor; with_client remains private and fields immutable.
No new session, policy, authority issuer or alternate implementation is created.
Dispatch/completion/reattach/seal all use the retained client's LocalActorRef.

Each construction has a fresh client dispatch mutex; cloning its client shares
that mutex. Multiple projections still submit to the same actor mailbox, whose
seal/admission logic owns cross-client ordering. Seal deliberately does not wait
on the client mutex, so late dispatch encounters the actor-owned fence. The
existing LocalActorRef capability/constructors are unchanged: this is not a new
attestation of arbitrary caller-built Rust handles or endpoint/actor pairs.

The added external integration assertion constructs the public projection after
real actor retirement and checks ActorExited for that exact identity; the existing
independent live sibling is then invoked successfully. It is meaningful terminal
routing coverage, not proof of new live host composition or all-resource cleanup.

Inspected immutable candidate diff, production producer, endpoint delegation and
ResidentToolClient gate/dispatch ownership. git diff --check 2db8f2c6 d393cbb8
passed. No review test/build was run or duplicated; no actor_host/manifests/native
edits. Host composition must still derive its canonical projection from its owned
actor instead of accepting independently supplied endpoint and actor values.
