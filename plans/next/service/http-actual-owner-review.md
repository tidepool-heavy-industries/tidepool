# Actual-owner HTTP seal review

Reviewed76ffaf09 and required test repair; accepted repaired sourcea04a4412
for the actual-owner HTTP consumer scope. Private actor evidence is returned
through installed ResidentInteractivePolicy from a real authored rootDriver;
no successful mock seal or exposed constructor is used.

Repair replaces arbitrary JSON substring42 with success, exactly one inputText
content item, and exact standalone transcript42. Startup/event receipt, late HTTP
join, foreign seal, forest shutdown and server/hosted joins are bounded. Exercise
panic is caught; artificial gates are released, server draining requested and
retained tasks/forest cleanup attempted before original panic is rethrown. Timeout
abort is recorded as failed cleanup, not successful drain. Completed failed joins
are marked consumed before unwrap, avoiding repeated polling during recovery.
Source inspected failure paths; implementer reports an observed incorrect-wire-key
assertion panic with empty cleanup-failure list before correcting the test. This
review does not claim comprehensive injected cleanup failure coverage.

The successful Haskell40+2 exchange is real. Late HTTP-admitted dispatch is delayed
before reaching the real endpoint, then released after its real seal; actor rejects
it. The seal timeout is deliberately before real barrier delegation; it does not
prove actor-internal active-effect wait scheduling. Exact real actor ID and
incarnation are checked, including foreign expected values against genuine evidence.
Correct-thread /completed succeeds while quiesced; wrong-thread callback rejects.
No pending native fork group is created here: callback success with empty ready
groups does not establish native durable-call correlation or closed-prefix release.

Cleanup is test-local real forest shutdown plus HTTP/hosted task awaits, not the
production host's process/custody retirement. No native controller, actor-host
wiring, actor/runtime definitions, manifests or live host were changed by review.

Independent exacta04a4412 execution: `NEXTEST_TEST_THREADS=1 just test-lib tidepool
' test(http_actual_resident_seal_identity_late_dispatch_and_completion) '`
(without padding) passed the single selected actual-owner test. Full tidepool lib
test target compiled and private compile daemon teardown observed. No lifecycle
suite duplicated. Evidence retained reviewer target/http-actual-review/{repaired.log,
hash.txt}. Formatting and diff checks passed. Parent integration remains separate.
