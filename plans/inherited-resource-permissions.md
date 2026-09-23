# Inherited resources: observation without control transfer

## Accepted contract

All Haskell bindings inherit unchanged. A concrete handle retains its identity;
fresh computations execute as the caller. Sharing a handle permits inspection,
not interference. Runtime resource owners enforce permissions per operation.

- Non-consuming reads, including results, output, progress and diagnostic status,
  work across actors. This is control safety, not information sequestration.
- Control remains with the existing owner, producer, or explicit control grant.
  Resolve authority in the runtime, never from a caller-supplied owner field.
- Cursor values copy freely. Advancing a resource-stored cursor or draining
  someone else's subscription is control. Create independent listeners instead.
- Inspecting a foreign watch does not subscribe the inspector, suppress the
  owner's notification, consume a result, or execute the owner's callback.
- References do not extend resource availability. Release wakes outstanding
  listeners as unavailable. Existing protections against forgetting active
  requests remain. Previously extracted values survive by Haskell reachability.
- A successful read captures its value atomically with the availability check.
  Release after that point cannot revoke the returned value. A queued Ready
  notice or internally ready listener is not an extracted value.
- Nested handles retain resource rules. An extracted closure creating fresh work
  runs in the receiver's context; a closure reading a released captured resource
  fails on that new effect.

No read-grant tokens, automatic permission requests, ownership transfer, STG
pruning, permission type parameters, or universal resource registry. Agents may
ask owners to perform control operations. Preserve existing integration grants,
effect availability requirements, and filesystem isolation.

## Implementation

Classify exposed operations in their existing owners as observation, caller-owned
listener management, or resource control. Enforce checks at owning entry points,
including raw/generated effects and hosted tools, not duplicated Haskell checks.

Requests, progress and watches: share non-consuming observations and permit
caller-owned watches on foreign responses. Keep request updates, settlement,
cancellation, abandonment and release under existing authorities. Foreign
listeners must neither suppress owner notifications nor prevent authorized
release. Release invalidates dependent observations and publishes wakeups.
Retain shared result cells unless focused tests demonstrate a defect; do not
serialize live Haskell values or add another result store.

Commands: share status and retained output. Keep stdin, EOF, resize and
cancellation owner-controlled. Reading must not advance another actor's display
position. Reject consuming operations before mutation and offer an existing
non-consuming alternative, or add a peek through the same owner where needed.

Worktrees/events: share existing typed HEAD, branch, submission/dirty-state
observations and event sources. Preserve mutation grants and checkout bindings.
Do not add arbitrary access to another actor's live files or worktree deletion.
Subscriptions belong to their caller; foreign drain/unsubscribe is control.
Preserve event coalescing, no-replay, bounded queues and overflow behavior.

Replace `boundHead` with `currentCheckout`, without an alias: root project
checkout or child's bound checkout, resolved for the executing actor. Keep pinned
commits and explicit worktree sources distinct. Captured absolute paths stay
fixed; relative paths in fresh commands resolve in the caller's checkout. Source
resolution must be explicit and tested: source admission captures the revision,
not eventual child startup. Preview is prospective, not a reservation.

Use structured effect failures, never error-text control flow. Distinguish
unavailable resources from unauthorized operations. Include operation, resource,
owner and caller where known, with a real recovery such as peek, an independent
subscription or asking the owner. Do not promise recovery through a retired owner.

Make default response/watch and branch-preview displays compact while retaining
full values and `inspectFull`. Preserve base/submitted identity, missing evidence,
pending/unavailable states and dirty state. Compile examples of inherited helpers,
mixed structured replies/textual caveats, independent listeners, denied control,
and durable values versus expiring references. Remove blanket inherited-handle
refusal guidance.

## Acceptance criteria (including the hosted Sol review)

Use deterministic synchronization, not sleeps, for races. These are proposed
behavior tests, not claims that the interview exercised them.

1. Invoke one inherited command helper in root and child; each creates a fresh
   command in its own checkout. Explicit paths/commits stay fixed. Test root and
   child `currentCheckout`, relative command paths, and admission-time revision.
2. Complete a response after inheritance; independent actors obtain the same
   typed result, including closures. Passing a reference through a typed reply
   behaves like inheritance.
3. **Combined listener/release scenario:** parent and child independently watch
   the same pending response. Settlement wakes both without stealing the owner's
   notification or executing the other actor's callback. After settlement and
   producer closure, release before the child's read makes that read unavailable.
   A read accepted before release still returns its captured value. A queued Ready
   notification never promises continued availability. Check both orderings.
4. Copied cursor values and independent listeners do not consume each other's
   output/progress. Foreign draining refuses before mutation; peek or an
   independent subscription still works. Caller-owned draining succeeds.
5. Non-owners can observe but cannot cancel, publish, write stdin, release, or
   mutate worktrees. Existing owners/producers/integration grants still work.
6. Releasing observed resources wakes pending listeners unavailable rather than
   hanging or retaining the source implicitly. Do not relax active-request
   forgetting protections to manufacture the race.
7. Extract a closure that creates fresh work, release its source response, then
   invoke it in the receiver. Separately extract a closure capturing a job,
   release that job, and assert its later observation fails without retargeting.
8. Actor retirement preserves independently retained Haskell values, not access
   to released resources. Test nested handles and invalid/stale/wrong-kind and
   wrong-run references; no identity reuse may retarget a handle.
9. Worktree observations succeed without granting live-file access. Foreign
   subscription drain/unsubscribe fails. Event semantics remain unchanged.
10. Compact displays and compiled examples preserve failure distinctions. Teach
    a child's own watch over an inherited response, not merely foreign watch
    polling. Show expired-reference recovery as well as successful handoff.

## Delivery

Work only in the dedicated worktree. Keep reference/control enforcement,
listener/release changes, API migration/diagnostics and examples/acceptance in
distinguishable checkpoints. Regenerate authoritative bindings if needed.

Run focused supported-environment checks; compile affected targets, format and
run `git diff --check`. Defer `just verify`. Independently review authority,
cursor consumption, notification routing, exact incarnations and cleanup races.
Finish with fresh-workspace parent/two-child acceptance. Report executed checks,
compile-only targets and blocked checks separately. Leave parallel performance
work and the main checkout untouched.

## Acceptance notes for this revision

The focused parent/child tests cover inherited responses, independent watches,
release before and after accepted reads, typed handle handoff, and an extracted
closure that starts fresh work in its receiver after the source response is
released. The command tests cover distinct actor identities and checkouts;
the host-command test executes `pwd` through the caller-relative directory
resolver. The fresh `exomonad new` scaffold test uses the locally pinned
default-workspace commit. A live model run and the full `just verify` gate are
separate rollout checks.

Two limits remain explicit. `Cmd.Job` has no release operation: retiring its
creator cleans process resources but leaves the logical job registered for
status/output, so the proposed captured-job-after-release test cannot assert
unavailability until that lifecycle contract exists. RepoEvent subscriptions
and mailboxes likewise have no actor-retirement purge hook; creator checks do
not add one. A request carrying 2,000 evidence entries also exhausted the
engine's 100,000-step observation budget during a focused test; the retained
display test now uses 400 entries, still larger than its first-page budget.
