# Root disposition of custody validation checkpoint

Candidate 01a181b322833aab2e523f4594cebec13e3740f1 is retained on the service
branch, NOT merged into root and NOT approved for deployment. Root inspected
live-custody-validation.md, independent custody review and core custody/termination
diff. Detailed runtime and test receipts remain with service/custody specialists.

Service directly ran the strengthened exact-bootstrap test at e4a5e2e8556b6346695261b83ebaabd184471c70:
both sibling SessionReady events prove initial worktreeHead completion; nested
boundHead, leaf activation, typed reply/watch and in-process retirement/releases
are exercised. One passed test; 106 excluded. Earlier 13 focused checks at
467a546701cef058ead8388136b6ea45e593e83f cover negative/cleanup/authority paths;
production is unchanged since that revision, but these are not 14 distinct tests
rerun at final head. Final candidate adds documentation only after strengthened
review/test merge. All executions above are service-attributed from root's view.

Blocking gate: tmux absence/kill success does not prove exact process reap.
ForkWorkspaceCustody::process_may_exist currently fences release irreversibly;
this is intentionally fail-closed but incomplete for normal deployed lifecycle.
The existing service/process supervisor must provide an authoritative exact-process
termination/release transition, reviewed with failure/uncertainty and cancellation
paths, before source is accepted as deployable. Do not weaken the fence to make
cleanup look successful. Private in-process recursion is not native mounted-service,
observer, full-prefix, reconnect or actual live-root acceptance.

Current root/live host remains unchanged. Exercising corrected native-host behavior
requires matched rebuilt binaries and a user-controlled restart/canary. The live
historical denials' exact timing/cause is still not established.

The service report's outstanding client/notification decisions were already made
at d59126b8 and delivered in queued request18, service-approved-continuation. They
are not new user decision needs. Its progress and terminal watches remain retained.
The ownership request for usage (28) was observed queued behind continuation18;
queue ordering is a concrete coordination constraint, not proof of a deadlock.

UX findings to carry forward: explicit checkpoint-versus-final result sums would
make staged deliveries easier to classify; supply progress or a real escalation
route at lead creation; document unavailable parent handles rather than forcing
broad :browse discovery. Apply to future contracts, not retroactively to captured
response types. Avoid repetitive progress wakes for bookkeeping with no gate change.
