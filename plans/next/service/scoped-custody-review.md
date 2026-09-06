# Independent scoped custody review

Candidate `5c3f2376c0a6c7bea3adedfbe888867686c45032`, against scaffold
`19d6eeb1` (service `3c96f192`). Merged into retained reviewer branch without
rebase. Scope: ActorWorkspaceCustody changes, fork_workspace.rs contract, private
scoped_custody owner and tests. Other integrated wave changes were not re-reviewed.

Disposition: accepted as explicit **no-settle staging**, not production service
launch or custody-release acceptance. No blocking defect found within that scope.

The claim is checked/consumed under the exact custody mutex; wrong actor,
incarnation, missing installed ActiveBinding, duplicate/concurrent claim and
observed terminal fail before spawn. Generation authority is the private owned
ActiveBinding, not a caller's copied id or cleanup status. There is no constructor
accepting an external ServiceScope, and no release function accepting a sibling's
ServiceScopeCleanup. The owner invokes its own captured scope to obtain status.
True synchronous spawn Err occurs before successful process creation in the
inspected PreparedServiceScope implementation; only that path permits
ScopedNotSpawned. Its guarded transition cannot clear an intervening legacy fence.

Actor terminal is an explicit Option and preserves the first observation.
Stopped-namespace status distinguishes actor-active from host-work-pending.
HostWorkQuiescence remains uninhabited. No copied status, successful stop or Arc
Drop is presented as observed scoped binding settlement.

## Independent execution at exact candidate

- `NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(scoped_custody)'`:
  4 executed, 4 passed, 119 excluded (0.820s test interval).
- Exact-name selection of `actor_host::custody_tests::custody_is_exact_and_released_only_after_last_owner`
  and `actor_host::custody_tests::custody_retains_binding_when_process_cleanup_is_uncertain`:
  2 executed, 2 passed, 121 excluded (0.231s).
- Changed tidepool lib-test target compiled. Both runs observed compile-daemon
  teardown. Formatting and scoped diff checks passed. Implementer's Shoal build
  remains attributed compile-only evidence; reviewer did not build/launch Shoal.

Evidence: `target/custody-review-evidence/scoped/` in reviewer worktree
`wt-9b9de707-fca5-4acc-af53-a57bb66e1ec6`: test.log, legacy-tests.log,
identities.txt, format.log and test-list.json. The listing confirms executable
`tidepool-49a97cc2c72bc78d`; SHA-256
`57c74c362456d3e13e5aebaa161f24821361b4d4f8cb063d9c3dd2eddd6d8c44`.
Real bwrap SHA-256 `b70e240c8de0b8f68d93d98f50040ad45a2a5d9b36de2f70cb90116e57d1d4fe`.
Full paths/other identities are in identities.txt. Initial Nix-shell listing
included shell banner text; a native listing within this existing environment
produced the retained JSON. Listings are not additional behavior tests.

## Explicit limits and integration obligations

Drop intentionally forgets the entire ScopedResources allocation, including the
custody Arc, process handles and descriptors. That is an **indefinite unrecoverable
in-process resource leak**, not a registry-backed retained/reclaimable owner. Even
successful namespace cleanup does not release the allocation or BindingTable
custody. This was explicitly permitted for staging; do not enable production
launch until quiescence and observed settlement have an owning recovery path.

Lost async result is executed only after process cleanup; pre-pin result loss
and potential stranded blocked init are source-inspected, not executed. Pin
failure uses an exited real `false` wrapper, not a bwrap permission/identity denial.
No payload is released, native inference invoked, or kernel binary attested.
Legacy process_may_exist is irreversible but is not mutual-exclusion admission
for legacy launch: a later legacy call can still fence an already claimed scope.
The staged path has no production launch caller; eventual composition must not
launch both modes for one custody. A prepared boundary is host-trusted input;
this layer does not independently attest its workspace or bwrap binary.

No BindingTable durability, HTTP/resident-work drain, inbox or native controller
acceptance is established here. Existing process-release and external-pin gates
remain in force. Reviewer made no production changes.
