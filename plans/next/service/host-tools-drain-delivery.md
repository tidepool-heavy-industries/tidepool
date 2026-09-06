# Hosted HTTP lifecycle implementation

Candidate7ac6079b integrates production71024341 and test5eb338e2. Public-to-host
control is acquired with service.control() before spawning service.serve(listener).
quiesce() irreversibly fences new calls/session binding; completion and registration
remain available, including over fresh connections. drain() fences all admissions
and asks axum to gracefully drain accepted HTTP requests. Phase watch lock is the
admission/transition linearization point; no guard crosses endpoint await.

Parent must retain server JoinHandle and await through &mut on bounded timeout.
Timeout or aborted connection is not successful HTTP drain. Successful HTTP drain
is not resident retirement: ResidentToolClient submits typed work to the existing
actor owner; dropping its waiter does not establish cancellation or undone effects.
No request ledger or process launcher was added. Existing serve remains live until
control is used; actor_host wiring remains parent-owned and unchanged. Explicit
allow(dead_code) marks methods pending that host consumer, not unused mechanisms.

Direct verification at7ac6079b: NEXTEST_TEST_THREADS=1 just test-lib tidepool
' test(host_dynamic_tools::drain_tests) | test(host_dynamic_tools::tests) '
(without outer padding):12 passed,110 excluded, nextest
 d3ace3c3-861b-4042-b89b-a81e252deb09. Full lib target compiled; private compile
daemon teardown observed. Binary SHA256
e729c38d6429f937242068f6c757f1fa5c81d00c503d843b46de3a31ecea72ff.
Logs target/host-drain-owner/{compile,focused,integrated}.log retained in owner
worktree. Formatting and diff checks passed. Child's exact three-test evidence is
retained through httpTestsResult; httpTestsWorker retained for repair.

Parent independent review remains required. Tests exercise real Unix HTTP and a
gated endpoint, not native controller, real Haskell cancellation, deployment or
custody release. Client disconnect test explicitly releases endpoint separately;
it does not pretend a dropped HTTP client settles resident work.
