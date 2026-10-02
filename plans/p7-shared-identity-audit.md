# Shared identity across actor machines: retained finding

This is a historical parcel-7 audit, not a current-source acceptance result.
The finding matters to fresh-machine and M2 acceptance because Haskell
`ExitCell` values are mutated in place. The old audit confirmed that mailbox
values can cross machine sessions as heap-copy parcels; a captured cell cannot
be assumed to share identity across that boundary.

## Confirmed historical defect: typed request response

The audit reproduced `progress_retains_closures_and_watch_snapshots_across_calls`
failing when `pollResponse` observed a ready request but the caller's
`Response`-owned Haskell cell remained empty. `Response` is created by
`newRequestHandles`, filled by `runRequest`, then read by `pollResponse` after
Rust reports `RawResponseReady`. Rust's `ObserveResponseWith` handler now
provides registry-backed readiness, but the Haskell reader still reads the
captured `ExitCell` and treats an empty value after readiness as an invariant
failure. Recheck this path with an executed cross-machine regression on the
current joined revision before calling the defect fixed or present.

## Unverified related risk: actor completion

The old audit found the same closure-captured `ExitCell` shape in
`Tidepool.Actor`'s `ActorRef` exit value and in agent launch helpers. A
`SelectedContext` launch can use a dedicated machine, so a parent-side cell
may not share identity with a child-side fill. This was a code-shape finding,
not a reproduced failure. Current acceptance should prove completion through
`awaitExit`/`pollExit` for both direct actor starts and agent starts on an
independent machine.

`Async` handles are normally scheduled inside one prepared program; the audit
found no ordinary cross-session use. `AgentRef`'s placeholder cell is
permanently pending. Progress, watches, and replies otherwise use Rust-tracked
request state and their explicit observation paths.

The detailed parcel-7 trace and review discussion were removed from the active
index; Git retains them. Current ownership and gates are in
[final delivery](engine-harness-final-delivery.md). Neither historical
observations nor isolated tests establish M2 acceptance.
