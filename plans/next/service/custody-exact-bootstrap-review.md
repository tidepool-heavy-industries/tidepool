# Exact bootstrap acceptance review

Reviewed and independently tested: `6028ed0a5c4668c54919312500d82eeffb19991b`,
relative to accepted `467a5467`. Only the strengthened custody test and adjacent
Haskell fixtures are in this acceptance delta. Review branch incorporated the
candidate by fast-forward merge, without rebasing.

Disposition: accepted for the stated in-process bootstrap regression coverage.
Two sibling actor/request identities must each reach SessionReady after custody
installation and fork gate acknowledgement. The production RunRequest evaluates
its target worktreeHead before requestSessionSited, so observing these exact
activations covers the previously unproven first request use, not just attachment.
Nested unfold is dispatched through the actual first sibling policy with boundHead;
an additional parent commit distinguishes its seed from the other sibling.
The leaf's exact activation, respond, owner watch and typed reply are checked.

Review repair removed silent event discards: before shutdown, unexpected events
fail; after shutdown, an unordered map requires exact root/two-sibling/leaf
retirements with expected cancellation terminals. Delivered ChildExited events
are checked but not required from shutting-down mailboxes. Retained terminal
values must match. All three binding releases and retained source files are
asserted. No additional production or native behavior was introduced.

Independent command at exact `6028ed0a`:

```
NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody_precedes_first_bootstrap_worktree_use_for_two_siblings)'
```

Changed test target compiled; 1 test executed and passed in 29.824s, 106 excluded.
Nextest run `69c60f95-305e-4b50-97c3-409d25377cf0`; compiler daemon teardown
observed. Same single test had independently passed at preceding `ab4e9949`,
before the event/cleanup repair. Formatting and diff checks passed at `6028ed0a`.

Evidence in reviewer worktree `wt-9b9de707-fca5-4acc-af53-a57bb66e1ec6`:
`target/custody-review-evidence/exact-event-repair/{test.log,identities.txt,format.log}`.
Test binary SHA-256: `9766a6e18bcea68cd300b50440cdbcab9c285998749a85406bef11817d1ce1a5`.
Extractor SHA-256: `463d2664aea5b9e776efacd1ed7d1659735998caf340c17c676cb375401e2c93`.
Local built worker SHA-256: `97ebfb1e366c0511cf923d8f2fbd9465a9bff95a7c6c7c540281934b1fdac51d`.
The worker hash differs from implementer's reported build; these are independently
recorded reviewer identities, not an assertion of identical worker binaries.

Limits: real Rust/Haskell resident execution, not native inference, mounted
service/TUI behavior or exact OS process reaping. Process-release integration gate
is unchanged. Prior negative/cancellation tests were not rerun in this narrow
review, and the parent's earlier test totals do not establish this new coverage.
No running host, dependency pin or production source was modified.
