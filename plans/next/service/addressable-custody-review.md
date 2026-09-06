# Fresh addressable custody review

Reviewed original `1b2ca932` then repair `980a232c1d8c7bf14dfda79386e146a95341c488`
against scaffold `3cb76b46`. Disposition: accepted for staged addressable ownership
and exact host-only process recovery; not launch/settlement/product acceptance.
This supersedes prior scoped review acceptance of unreachable `mem::forget`.

## Review and repair

The existing pending map is now InteractiveOwners, anchored by actor_host::run
before spawning the fleet. Slot reservation records the exact installed custody
Arc before spawn; spawn writes its own synchronous result to that slot before
notification. No scope/slot owns a custody back-reference or task handle. The
map survives Retired, late launch completion and retirement-result loss. A late
completed deployment after actor retirement goes directly to owned retirement.
No second scope registry or leaked allocation remains.

Initial teardown test constructed RetainedInteractiveFleet manually and its
private printable error had no recovery entry outside actor_host. Repair puts
the actual run retention predicate/result construction in handoff_application_owners,
preserves original resource-bearing errors, and makes the carrier recoverable by
crate-private from_error plus exact-actor deadline-bounded Observe/Pin/Stop.
No command release, host-work success flag or custody-settlement operation exists.
Tests now use this production handoff and Box<dyn Error> roundtrip, for completed
and timed-out fleets, with notification receiver dropped before spawn/pin.

Inspected real propagation: shoal::host returns actor_host::run's Box unchanged;
Shoal CLI propagates the result. There is no automatic CLI/operator recovery
policy: crate-internal owner access is staged for subsequent host composition.
Returning an error and exiting the host loses live handles; neither code nor
this review claims continuity through host death. Ordinary cleanup errors still
preserve concrete identity and precedence. Unresolved scoped/legacy custody
remains an error/retained row, not successful settlement.

## Independent execution

At original 1b2ca932: exact lost-result and concurrency/sibling/legacy tests ran,
2 passed, 125 excluded. Initial cold build completed; compile daemon torn down.

At repaired exact 980a232c, with explicit SERVICE_SCOPE_BWRAP and
NEXTEST_TEST_THREADS=1, ran exact tests:
- scoped_custody_production_handoff_recovers_completed_and_timed_out_fleets
- scoped_custody_handoff_preserves_unretained_results
- scoped_custody_lost_spawn_and_retirement_result_remain_addressable

All 3 passed, 126 excluded, 0.481s test interval; changed tidepool lib-test target
compiled. Nextest run a32b96da-0dc6-43b2-abcd-927c118e2f88. Real blocked bwrap
namespaces were pinned/stopped from recovered ownership; payload remained gated.
Fixture custody Arc counts returned to fixture-only (no unreachable handle leak).
Compile daemon teardown observed. Formatting and diff checks passed.

Evidence in this reviewer checkout: target/addressable-review-evidence/
{initial-tests.log,repaired-tests.log,identities.txt,format.log}.
Implementer's eight-test selection and Shoal executable build remain attributed,
not independently rerun here. Hashes identify this review's exact local binaries.

## Remaining boundaries

Host HTTP/resident-effect quiescence and observed BindingTable settlement remain
unavailable; no release/rebind workaround. Native scope launch selection remains
disabled. Prepared mount and bwrap are trusted host inputs, not attested here.
No full run/provider/CLI scenario, native inference, kernel binary attestation
or host-death recovery was executed. Pin-error injection remains exited-wrapper,
not real bwrap permission/identity denial. Socket/build/inbox/host-tools owners
were untouched by the repair. Further integration must preserve map lifetime,
exact generation and terminal observations rather than substitute copied status.
