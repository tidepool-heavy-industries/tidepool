# Independent root preparation acceptance

Reviewed service candidate2db8f2c6 and merged it with root1f41b455 in an isolated
acceptance checkout. Exact integrated and tested source:
62e99ec62c81404259e0c1348e373734584bc3ec.

Disposition: safe to integrate as PREPARATION, with the explicit limitations below.
No blocking defect found in the reviewed bounded slice. This is not mounted native
service acceptance. Later unsafe host cleanupb51745d8/canonical repair is excluded.
No production files were repaired by this reviewer; merge was clean.

## Review

Inspected the production diff and owning consumers for pre-bootstrap binding
installation, exact actor authority, startup admission/cancellation, child
forget/snapshot ordering, hosted seal and component shutdown, notification handoff,
inbox pre-send fencing/reopen, HTTP admission and immutable endpoint control,
exclusive socket/build allocation, namespace gate/process identity, addressable
host-map retention and real boxed-error handoff. Existing root durability owners
and uncertainty fences are unchanged. Generic behavior reports Unsupported realm
cleanup; terminal-only/forced exits do not manufacture cleanup confirmation.
Correlated completion stays open while sealed, then closes before child accounting.
Hook failure is retained while safe exclusive realm cleanup is attempted.

Legacy launch still renders the existing native command and wraps it through the
existing mount/tmux path. Production notification send explicitly returns
Unavailable before tracked publication. New scope/seal primitives have documented
staged consumers rather than fabricated backend success. Legacy process/socket/
build/binding cleanup deliberately retains uncertainty; namespace status is not
accepted as hosted-work or binding-settlement authority. Exact private evidence
must still be composed by the later host owner; the separate actor/endpoint
construction issue in the excluded host candidate is not claimed repaired here.

## Direct checks at62e99ec6

All commands ran sequentially in repository Nix/toolchain setup. Each just test
invocation reported private compile-daemon teardown. No broad battery ran.

| Command (NEXTEST_TEST_THREADS=1 for just) | Executed result | Log |
|---|---|---|
| just test-lib tidepool-node 'test(strict_inbox_faults_propagate_without_retry)' | 1 passed,44 excluded;20 hit-checked injected fault cases | inbox.log |
| just test-lib tidepool-actor 'test(startup_admission_cancellation) \| test(authored_seal) \| test(cleanup_child_force) \| test(unsupported_endpoint_does_not_fabricate_seal)' | 4 passed,96 excluded | actor.log |
| just test-target tidepool-actor resident_local_actor 'test(local_actor_owns_resident_policy_children_and_terminal_reply) \| test(authored_failed_shutdown_hook)' | 2 passed,0 excluded | authored.log |
| just test-lib tidepool 'test(custody_precedes_first_bootstrap_worktree_use_for_two_siblings) \| test(http_actual_resident_seal_identity_late_dispatch_and_completion)' | 2 passed,144 excluded | host.log |
| just test-target tidepool-actor notifications 'test(notification_)' | 2 passed,0 excluded | notifications.log |
| just test-lib tidepool 'test(scoped_custody_lost_spawn_and_retirement_result_remain_addressable) \| test(scoped_custody_production_handoff_recovers_completed_and_timed_out_fleets) \| test(launch_shutdown_timeout_preserves_partial_success_and_reports_uncertain_abort) \| test(build_resource_reallocation_cannot_adopt_retained_directory) \| test(notification_admission_and_poll_preserve_typed_request_bindings)' | 5 passed,141 excluded | retention.log |
| cargo test -p tidepool-protocol --test shoal_control_contract notifications_have_one_way_admission_and_owner_receipt_observation -- --exact | 1 passed,5 filtered | protocol.log |
| just test-lib tidepool-node 'test(scope_gate_writer_close_does_not_release) \| test(scope_monitor_death_after_detached_readiness_requires_init_evidence)' | 2 passed,43 excluded | process.log |

19 selected tests passed, not a whole-tree count and not summed with prior reruns.
All owning selected test targets compiled. Additional compile-only checks:
`cargo check -p tidepool-node --example service_scope` and
`cargo check -p tidepool --bin shoal` passed. Formatting for all changed Rust crates
and git diff --check passed. Haskell facade/privacy and authored policy fixtures
were exercised by their actual extractor-backed tests. No extractor IR translation
or fixture serialization implementation changed in this slice.

Full logs, revision and exact executable SHA256 values are retained at
`target/preparation-acceptance/` in reviewer worktree
wt-8ebc24cc-b352-4718-9553-1a0f792e4572 (branch
shoal/next-shoal-wave/service-preparation-acceptance/branches/review-integrate-check).
The later report commit is docs-only; checks name the actual tested merge.

## Limits and next owners

Persistent native controller/client and hosted-call bridge are still unimplemented
in this slice. Native600be9df is delivered but no new native binary, mounted
controller/observer, durable-prefix fork release, provider call, replacement host,
or all-resource custody settlement was exercised here. Process tests use real
Linux/bwrap under the documented trusted-kernel/binary prerequisites; they do not
attest those prerequisites or prove physical power-loss behavior/cross-platform
support. Root may merge this preparation now and continue native/host composition
as separate reviewed increments. Preserve retained worktrees/evidence.

UX: activation plus branch check established reviewer identity without Haskell
inventory. Large multi-owner diff displays still occasionally truncated; bounded
owner projections were more useful. One sequential native job with retained long
waits covered validation without progress spam. No measured token/cache savings.
