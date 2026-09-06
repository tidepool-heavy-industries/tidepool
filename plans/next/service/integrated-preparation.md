# Service integrated preparation

Service integrated reviewed notification checkpoint b4fab475 and opt-in namespace
scope583de712, then incorporated accepted root5c4375e3 (including strict durability
owners4088cd94). Source baseline bd9cf056 preserves all three histories. Scope
public types and typed example consumer are exposed at e18992bd.

Notification review accepted interpreter/host request-binding isolation, exact
receipt provenance and the legacy queue barrier. Production send deliberately
returns Unavailable before publication. Its inbox durability is NOT accepted yet:
retained notification lead must incorporate the strict owner baseline, wire shared
parent establishment and test uncertainty/reopen/fault paths independently. Root's
separate durability lead owns BindingTable, EventJournal and LogWriter repairs.
No additional writer or live inbox migration is authorized.

Namespace review accepted an opt-in noncloneable process owner. Its proof combines
exact validated namespace-init exit and direct monitor wait, under documented
trusted bwrap/kernel prerequisites. Service directly ran 17 boundary tests at
e18992bd (12 scope, 5 legacy, 26 excluded), including omitted-self-hold positive
mutation, stale/sibling identity rejection and detached descendant cleanup. The
public typed example compiled and executed, reporting deliberate monitor kill137
and namespace drain. These tests do not attest the kernel binary or exercise a
real model-driven service fork.

Evidence retained in service worktree target/service-integration-evidence:
revision.txt, scope-tests.log, scope-example-build.log, scope-example-run.log.
Direct host notification integration at e18992bd also passed all three selected
tests (116 excluded), nextest9edeb333-5aa0-4ba4-891a-20cd63c85e21; full tidepool
lib target compiled and private compile daemon teardown was observed. Command:
`NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(notification_admission_and_poll_preserve_typed_request_bindings) |
test(notification_barrier_never_enters_legacy_push_or_batch_ack) |
test(native_push_acknowledges_only_after_acceptance_and_retries_the_same_row)'`.
Log: host-notification.log. These tests use authored interpreter/inbox handoffs
and scripted transport, not production native delivery. Neither source
incorporation nor a canary installs this into live Shoal.

## Remaining composition boundary

Legacy wrap/tmux still owns production launch. Its cleanup cannot authorize custody
release, so the previously installed process_may_exist fence stays conservative.
Do not connect a free-standing Copy ServiceScopeCleanup receipt to release: an
unrelated sibling receipt must never authorize the installed binding. A future
host owner must structurally retain its exact scope and custody together, including
uncertain pre-pin launch and host-side effects cleanup. The retained designer is
reviewing that seam before controller implementation.

Actual RemoteAppServerClient/controller integration, closed-call bridge, mounted
prefix/tool delivery, observer isolation and retirement require the matching native
revision from the human-managed external work. No external worker was forked or
steered, and no running host was replaced.
