# Independent resident lifecycle review

Reviewed e7c0b64b, requested startup/forget-accounting repair directly from retained
implementer, and accepted repaired6224f889 (production/test source4793ba37) for
parent integration within the actor/machine evidence boundary. Not mounted service
or all-resource retirement acceptance.

## Repair

Holding a startup admission read lease alone was insufficient: cancellation while
awaiting ractor pre_start dropped the lease without recording unavailable cleanup.
StartupCustody now records uncertainty before releasing admission on cancellation,
and disarms only after child registration or accounted failure. Parent shutdown
therefore cannot overtake the evidence. Existing child map lock now also serializes
forget/aggregate/removal with the child-plus-aggregate shutdown snapshot. No new
registry or detached work owner was introduced. The actual pinned ractor pre_start
await is inline; losing the caller is not proof that its startup effects cleaned.

The deterministic gated kernel-startup test exercises the read barrier, successful
registration, aborted startup waiter, retained uncertainty and post-close denial.
It is not an authored external-resource cleanup test. Child force is separately
unconfirmed; hook failure preserves failure while exclusive realm cleanup proceeds.
Reviewed exact actor identity, private evidence construction, irreversible hosted
Tool/Workbench/reattach seal, completion Closing before child accounting, retained
outcome despite waiter loss and generic Unsupported realm evidence.

## Independent execution

At exact6224f889:
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-actor 'test(startup_admission_cancellation) | test(authored_seal) | test(cleanup_child_force) | test(unsupported_endpoint_does_not_fabricate_seal)'`:4 passed,96 excluded; nextest0b088f52-069f-4471-b68c-cd518adf120a.
- `NEXTEST_TEST_THREADS=1 just test-target tidepool-actor resident_local_actor 'test(local_actor_owns_resident_policy_children_and_terminal_reply) | test(authored_failed_shutdown_hook)'`:2 passed,0 excluded; nextest30d35029-3680-4a88-9124-621d098c0a89.
- Both full owning test targets compiled; private compile-daemon teardown observed.
  cargo fmt -p tidepool-actor -- --check and git diff --check passed.

Evidence directory (retained reviewer worktree):
/home/inanna/.cache/tidepool/shoal/actor-worktrees/f79d0ae899951577b2fea06ed8f088919d48f5f01e791513abffefa7fbf9fe02/worktrees/wt-5b6c41ba-f682-4b2c-b60d-5718c924dfe4/target/lifecycle-review-evidence/
Files: repaired-unit.log, repaired-authored.log; candidate-unit.log preserves the
three passing checks before the repair (not claimed as repaired evidence).
Actor test SHA256 a5c614a551c0a7d89de9ae45f778fd5bbc0b3f11ec8d43baa5e6bfdbae2afca7.
Authored target SHA256 b5e47e27ea2fd652f08b98410861d36f5d0e66d13d1d5d72087eafb0e44cc668.

## Remaining gates

Authored tests exercise real endpoint active work/late dispatch/waiter loss and
resident Haskell success/failed hooks. Completion admission while sealed is tested,
not a mounted native durable-prefix fork release-versus-abort campaign. Kernel
startup/force tests are not authored forced-external-effect cancellation. No generic
ActorTerminal or HTTP drain attests arbitrary external handlers, processes, sockets
or build resources. Parent must combine exact separate ownership evidence; none
of this enables native launch, custody release or a live-host replacement alone.
