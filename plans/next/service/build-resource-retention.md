# Build resource retention boundary

Accepted narrow source repair c29cb396 plus independently reviewed allocation
repair734df705; integrated service fe41095c. The existing actor build-directory
owner now fences deletion before uncertain tmux submission, including error,
timeout and Drop. Explicit deletion failure cannot trigger a silent Drop retry.
Exclusive leaf creation is essential: create_dir_all previously allowed adoption
of retained same-key files as an unsubmitted lease, bypassing that fence.

Same-key retained directories now fail AlreadyExists. This intentionally prevents
unsafe automatic relaunch/recovery; deliberate reuse requires exact cleanup and
ownership proof, not clearing the fence or retrying allocation. Parent directories
are still created as before. No new path resolver, registry or cleanup scheduler.

Service inspected exact repaired diff and production path helper and directly ran:
`NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(build_resource_)'` at fe41095c.
Four tests passed,119 excluded; nextest2276b83b-bddb-4d8b-a934-d73e0ddb4b03. Full
host library test target compiled and private daemon teardown observed.
Evidence: service target/service-retirement-evidence/build-retention-integrated.log.
Independent reviewer evidence at wt-9392f185-065e-41af-993e-c032b4699aba,
.shoal/evidence/build-retention-review/tests.log (four tests at734df705).

No live tmux launch or actual partial-deletion syscall fault was exercised here.
The no-retry branch was source reviewed. Socket directory ownership, host HTTP
and resident-effect draining, and exact scoped custody settlement remain separate
active work. No running host was replaced and no native launch switch enabled.
