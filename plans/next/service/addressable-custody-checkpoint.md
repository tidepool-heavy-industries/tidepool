# Addressable scoped-retention checkpoint (not implementation acceptance)

The unreachable `mem::forget(ScopedResources)` candidate is rejected. No-settle
permission does not authorize unreachable ownership. The existing reviewed
namespace owner remains useful, but the host co-owner must be repaired before
service integrates/enables it. Current merged custody head contains that rejected
staging code; it is not an accepted production baseline.

## Existing owner trace

`LocalResidentInstallation.worktree_custody` and the kernel retain the installed
lease, but those owners do not retain it after actor terminal plus installation
consumption. `PendingInteractiveLaunch` currently stores only cancel and fork_gate.
The entry is inserted before launch task spawn, making it the right existing
addressable owner. `launches: JoinSet` is completion transport, not ownership.
`InteractiveDeployment.worktree_custody` becomes available only after the launch
result is collected; it cannot cover lost/late results before collection.

The Retired arm removes pending entries immediately. Launch success/cancel/error
arms also remove them; fleet shutdown drains the map before waiting/aborting
launches. These are precisely the lifetime transitions that would destroy a
naively added slot. RetainedActorExit records terminal observations, not process
owners; adding process handles there would cross the runtime boundary.

## Concrete service-owned changes needed

Reuse the existing pending-launch map; do not add a second scope registry.
Extend its row before spawning with a noncloneable host retention owner:

```rust
struct ScopedHostRetention {
    custody: Arc<ActorWorkspaceCustody>, // existing exact installed generation
    slot: Arc<Mutex<ScopedProcessSlot>>,
}
enum ScopedProcessSlot {
    Reserved,
    Spawning,
    NotSpawned(ServiceScopeError),
    Owned(ServiceScope), // same synchronous spawn writes here BEFORE completion
}
struct PendingInteractiveLaunch {
    cancel: Option<oneshot::Sender<()>>,
    fork_gate: Option<ForkGroupGate>,
    scoped_retention: Option<ScopedHostRetention>,
    // typed launch/retirement state, not rendered error text
}
```

The slot never owns retention/custody or a task handle pointing back to itself.
The blocking spawn closure owns only a slot clone and immutable prepared inputs;
it writes its exact result into that already-registered slot synchronously before
notifying completion. Dropping the async receiver cannot lose the process owner.
Stop/recovery operates by exact actor lookup of the existing map row and borrows
its `Owned` scope. No external cleanup receipt can enter this API. Scope errors
leave the scope in place; status and observed actor terminal stay separate.
A `Spawning` row cannot be retired merely because its async awaiter disappeared.

Required narrow signatures in private scoped module:

```rust
fn reserve(custody: Arc<ActorWorkspaceCustody>, actor: ActorRef)
    -> Result<ScopedHostRetention, ScopedClaimError>;
fn spawn_into(slot: Arc<Mutex<ScopedProcessSlot>>, prepared: PreparedServiceScope,
              env: ServiceEnvironment, output: File); // stores Result, no scope return
fn stop_owned(owner: &mut ScopedHostRetention, deadline: Instant)
    -> Result<ScopedCleanupObservation, ServiceScopeError>;
```

Service must choose and implement the map lifetime: keep its exact row through
launch success, actor retirement, late results and pending HTTP/resident work,
and remove only after explicit observed settlement (or definitive pre-spawn
rollback). A moved deployment must not leave a retention gap. Existing retirement
workers may borrow/clone only the slot; the map remains the anchor even if their
result is lost. Full fleet teardown must expose retained/unconfirmed rows to its
owning host rather than drop them while reporting successful cleanup. Host
process death cannot preserve live pidfds; it remains a separate uncertainty.

These changes touch `PendingInteractiveLaunch`, `run_interactive_applications`
insertion/Retired/join/teardown arms, deployment/retirement handoff, and possibly
the fleet result contract: outside this worker's authorized custody sections.
Service must own this composition change or explicitly transfer those exact
regions. Custody worker can own the private slot/claim implementation and tests.
No host-tools/inbox/socket/durability owner edits are requested.

## Deterministic acceptance after handoff

Register row, start blocking spawn with controlled notification boundary, drop
receiver BEFORE pin, recover through row, pin and stop exactly its own blocked
bwrap namespace. Verify slot stays addressable while actor active, after actor
terminal, and while host work pending; test lost retirement result and duplicate
stop. Fail pin/timeout without removing scope. No payload release/native switch.
Host-tools lead must provide its genuine HTTP/resident quiescence contract before
settlement. BindingTable's actual settlement result must be observed; no bool,
Arc Drop, receipt substitution, or rebind workaround. Tests may explicitly reap
fixtures but cannot use unreachable leaks to satisfy retention.

No test/build was run for this checkpoint; only merge/source/diff/format checks.
