# Independent custody review

Reviewed candidate: `4d473b407b94d96742ae1d802cf9b30666f11f6c`.
Production/test revision: `734c30fd2b98b74129cc140f9a86e6c27633551c`.
Reviewed against scaffold `f0a408ed` and custody assignment.

Disposition: pre-bootstrap custody and cancellation slice is suitable for staged
service integration, **not standalone deployment or full custody acceptance**.
After any tmux process submission, bindings are deliberately retained even when
pane cleanup succeeds. The service owner must supply exact-process supervision
and termination proof before releasing/reusing them. This is a blocking product
integration gate, not a cosmetic limitation.

The first candidate incorrectly cleared its process fence after `kill_pane`
returned Ok. That owner returns Ok for missing or non-owned panes; even actual
pane deletion is not observed process reaping. Repair removed both release APIs
and reports cleanup as unconfirmed. No new serialized cleanup variant remains.

Repair also addressed actual cancellation while installation is blocked: exact
shutdown intent is retained separately from published terminal in the existing
exit owner, then checked at bootstrap boundaries. Entry cannot proceed to provider
publication after observing that intent. Inspection traced last-owner release
before process submission, failed initialization, duplicate binding rejection,
actor shutdown and host launch/abandonment/retirement. Parent timeout's preexisting
forced-terminal publication is not strengthened into OS termination evidence.

## Independent checks

At `45360ce1`: sibling bootstrap test executed and passed (1 selected).
The wrapper then failed because zsh reserves `status`; the retained test log and
nextest summary establish the test result, not a successful wrapper exit.

At `4d473b40`: command below executed 4 tests, all passed, 103 excluded; compile
daemon teardown observed. These exercise actual cancellation before and after
binding, Haskell initialization failure after binding, and real private-tmux
missing/foreign pane behavior. No provider/native model inference was executed.

```
NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody_actor_cancellation) | test(custody_missing_or_foreign_pane) | test(custody_haskell_bootstrap_failure)'
```

`cargo fmt --all -- --check` and `git diff --check` passed at the revised candidate.
The changed tidepool library test target compiled independently. Implementer's
reported 12 focused tests and Shoal binary build remain attributed evidence;
reviewer did not rerun that entire selection or build/launch Shoal.

Retained independent evidence: `target/custody-review-evidence/` in reviewer
worktree `wt-9b9de707-fca5-4acc-af53-a57bb66e1ec6`: `siblings.log`,
`repair-checks.log`, `binaries.sha256`. No source/runtime changes were made by
reviewer; this report is documentation only. Native service/observer acceptance,
normal process reap/release, and integrated service cleanup remain unverified.
