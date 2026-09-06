# Resident lifecycle lead integration

Accepted reviewed0e5415e6, incorporating repair6224f889/source4793ba37. Exact lead
checked revision0e5415e60b39f0f622cc7b67fca0dd62e77746a2. No service/native deployment.

Direct Nix/toolchain checks, sequential:
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-actor 'test(startup_admission_cancellation) | test(authored_seal) | test(cleanup_child_force) | test(unsupported_endpoint_does_not_fabricate_seal)'`:4 passed,96 excluded;0d906b41-6ed3-42b8-8710-a18d8eefd9d1.
- `NEXTEST_TEST_THREADS=1 just test-target tidepool-actor resident_local_actor 'test(local_actor_owns_resident_policy_children_and_terminal_reply) | test(authored_failed_shutdown_hook)'`:2 passed,none excluded;758d4752-ea30-471d-b077-6a8da4333278.
- `nix develop --command cargo check -p tidepool --lib`: compiled downstream consumer.
- Actor formatting and diff checks passed; both private test daemons torn down.

Evidence root:
/home/inanna/.cache/tidepool/shoal/actor-worktrees/f79d0ae899951577b2fea06ed8f088919d48f5f01e791513abffefa7fbf9fe02/worktrees/wt-d46cefb0-5b6f-4757-9520-1f8e8ea261b0/target/resident-lifecycle-evidence/
Logs: integrated-unit.log, integrated-authored.log, integrated-host.log.

Independent review found and repaired cancellation during startup admission:
uncertainty must be recorded before its read lease releases. Exact child-map and
forgotten-outcome snapshot ordering is now serialized. Accepted only within the
actor/machine proof boundary; see resident-lifecycle-review.md for review evidence.
Seal does not cancel external effects. Generic terminal, HTTP drain and unknown
child/realm evidence never become all-resource cleanup proof. Mounted native
prefix completion remains gated; force/startup tests are actual kernel behavior,
not authored external-effect cancellation. No process/HTTP/socket/build claim.

The initial capacity rejection created no worker and did not block local work:
retained implementer completed authored tests locally. A later independent review
was admitted after preview. No retry/widening of authority or inferred token savings.
Implementation and reviewer remain retained for service integration repairs.
