# Resident lifecycle candidate

Source revision 437c7c25 follows scaffold dc2dbd36 and root approval a3e661fe.
This is a candidate for parent independent review, not service deployment or
external-resource cleanup acceptance.

## Owning contracts

- LocalActorRef::seal_hosted_work and both resident endpoint projections use an
  acknowledged mailbox operation. Open -> Sealed rejects Tool, Workbench and
  reattachment at the actor owner, including late dispatch from client clones.
  Completion/deferred fork release remains admissible while Sealed. Closing is
  irreversible and explicitly rejects completion/release before child accounting.
- HostedWorkSeal has private construction and exact actor identity. An unsupported
  endpoint returns Unavailable. Repeated observation cannot reopen admission.
- The existing RetainedActorExit retains component evidence; shutdown_with_cleanup
  returns terminal plus exact ResidentCleanupOutcome. A terminal-only/forced exit
  yields uncertainty, never retroactively confirmed cleanup. Abandoned async
  shutdown waiters do not cancel the actor-owned shutdown operation.
- Hook, realm and child outcomes are separate. Generic KernelBehavior defaults
  to Unsupported realm evidence, so generic success cannot certify a resident
  realm; explicit resident behavior supplies real checkout/realm results. Hook
  failure is retained while safe exclusive realm retirement is still attempted.
- Child timeout/force, unavailable proof, failed startup and forgotten terminal
  child without positive proof retain uncertainty. A monotone aggregate in the
  existing KernelContext preserves that uncertainty when routing is reclaimed;
  this is not another child registry. Child admission closes under the owning context write barrier before the
  snapshot; admitted startup holds a read lease through registration. Shutdown
  hooks and detached context clones cannot create new children afterward. No external handler/process/HTTP/socket/build proof is implied.

## Verification scope

The authored workbench test uses the actual private ResidentInteractivePolicy and
an authored NotifyWith call held at its real host handoff. It drops the caller,
proves seal cannot pass active ownership, rejects queued late work/reattachment,
permits correlated completion while sealed, checks sibling identities, and drops
a second shutdown waiter while work is active before observing retained cleanup.

The authored resident-policy tests execute successful child cleanup and a child
onShutdown error, retain parent child uncertainty and confirm the independent
sibling's Haskell closures remain usable. A GHC-free kernel test separately forces
a blocked child deadline, verifies terminal-only uncertainty, and ensures routing
forgetting cannot erase it. Unsupported endpoints and two existing kernel shutdown
regressions are separately checked. The force test is not an authored forced-
effect cancellation test. Completion callback admission is executed, but a mounted
native durable-prefix/fork release-vs-abort scenario is not executed here.

Private compile-daemon teardown is observed for focused commands. No live host was
replaced, no extra native work or dependency change performed. Initial failed test
attempts remain as evidence: missing Replies/Watches preamble imports caused
wrapper rejection; the first test incorrectly awaited PolicyInstalled from a
control-only new_workbench and was explicitly terminated. Tests now use the real
private endpoint directly and surface early invocation errors, without exposing a
public test-only constructor. A failed-hook fixture initially collided with the
foreign-session negative-test ID; that test setup is repaired.

Descendant-cap rejection was not an implementation blocker: this continuation
implemented and tested locally without further forks. Parent owns independent
review and service integration. Retained source/receipt proof remains narrower
than arbitrary external-effect quiescence, and service settlement must combine
its independently owned process, host, storage and native completion evidence.

The child-admission barrier is source-reviewed and exercised by existing child
shutdown paths; a deterministic concurrent detached-startup race test is not
authored here and remains a focused review/validation opportunity.

## Startup cancellation repair (4793ba37)

StartupCustody records unconfirmed evidence on cancellation before the admission
read lease is dropped. It disarms only after child registration or an explicit
failure evidence write. Forgetting holds the existing children lock through
aggregate update/removal; shutdown samples that map and aggregate under the same
lock order, preventing an omitted-child/stale-aggregate snapshot.

Pinned ractor 0.16.5 (Cargo.lock) actor.rs:796-826 awaits pre_start inline through
run_with_signal before supervision linking and before spawning the processing
loop; do_pre_start at 1129 directly awaits the handler. Dropping that future does
not run our retained shutdown protocol. Resource destruction is not inferred from
Rust future destruction; cancelled startup therefore remains Unconfirmed.

The deterministic gated ProbeBehavior test enters actual pre_start, observes
the admission write barrier unavailable, then either completes startup and
verifies registration before closing or aborts its waiter and verifies retained
uncertainty before closing. It also checks post-close rejection. Generic probe
realm evidence is Unsupported; this test intentionally does not fabricate a
Confirmed resource cleanup result. Existing authored seal and force tests passed
alongside it (3 passed, 97 excluded), evidence startup-repair.log. This is not an
authored external-resource cancellation proof.
