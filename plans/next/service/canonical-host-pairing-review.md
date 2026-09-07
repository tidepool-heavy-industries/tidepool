# Canonical host pairing: independent review

Production candidate `1f91d9aa4bd01358e394a6ff566ba189cf241aaf`, followed by
bounded test cleanup repair `5e21dbeb047b44809911c6f2375ecb6877098a36`.
Both incorporated without rebasing. Accepted for the assigned host-pairing scope;
this supersedes the endpoint-pairing blocker in host-resident-production-review.md.

Production `hosted_retirement::start` now accepts the owned actor plus HTTP
configuration/listener, not an independently supplied service or endpoint.
It derives the canonical policy internally through the actor owner's existing
public constructor. The same actor is retained for cleanup. Terminal shortcut
permission follows this construction, not a caller bool or endpoint label.
Arbitrary endpoint decoration is test-only and untrusted; even an initially
terminal expected actor requires exact endpoint sealing on that path.

The original stored control, seal/shutdown operations, service task/results and
owner-map anchor are unchanged. No second registry, shutdown retry, terminal
cleanup inference or native/final settlement capability is introduced. Failure
and timeout paths remain conservative. Obsolete separate BuildPolicy operation
was removed with the production constructor move.

The real initially-terminal foreign endpoint regression now rejects the endpoint
and retains HTTP while the foreign actor remains live. Trusted terminal cleanup
still drains without trying a stopped mailbox seal. The bounded review repair
moves negative assertions after explicit actor/service cleanup and joins the
original service task when present, including already-finished tasks; an already
consumed task is not unwrapped. This affects fixtures only.

## Independent evidence

At exact `1f91d9aa`, ran:

```
NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(actor_host::hosted_retirement::tests::)'
```

9 passed, 146 skipped, 64.510s; run
`0775180d-caaa-4bc9-9ecb-743a36e1446b`. Covers real authored live and trusted
terminal cleanup, initially-terminal foreign rejection, lost seal/HTTP waiter,
real gated pending shutdown waiter loss, foreign pending seal, unsupported seal
and failed-child uncertainty. Library target compiled. Log:
`target/canonical-host-review/tests.log`.

At exact `5e21dbeb`, rerun the affected
`hosted_initially_terminal_actor_cannot_drain_foreign_endpoint` test; exact result
and binary hashes are retained in `target/canonical-host-review/cleanup-repair-test.log`
and `manifest.txt`. Formatting and diff checks passed. The remaining eight tests
were executed at the preceding production-identical revision, not rerun after
fixture-only cleanup changes.

Implementer Shoal compile at `1f91d9aa` is attributed evidence inspected at its
retained checkout `target/custody-evidence/hosted/canonical-{build.log,manifest.txt}`:
Shoal SHA256 `ee5249795c87c7f5dfa6f4e54aa3db6f9c7808d47f02b22486c44f525f531575`.
The reviewer did not launch Shoal or replace the running host.

The previous external human-delivery gate is obsolete per current assignment.
Pin/build integration, native/external cleanup and final custody settlement
remain distinct obligations; this review does not claim those are satisfied.
Useful implementer context is retained for further integration repairs.

Final affected-test rerun passed: 1 passed, 154 skipped, 13.259s; run
`c7bacd5b-abe9-4ac3-9396-ac2c983bd7bc` at `5e21dbeb`.
