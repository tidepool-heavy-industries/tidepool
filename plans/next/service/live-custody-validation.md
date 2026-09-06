# Live custody regression validation

Root supplied two additional pre-implementation WorktreeUnauthorized failures:
`wt-ddb3c147-1550-4278-8b18-dd90a2f0a273` and
`wt-48a0d372-fe14-4430-9c79-1698c60946f9`. Run-map owns the original settlements;
service requested exact evidence through a retained typed peer request. These
identifiers alone do not prove the denial's operation or exact historical race.
Other branches have admitted descendants; universal recursive failure is not shown.

Custody repair remains the existing worker's obligation, not a duplicate branch.
Its original acceptance already requires deterministic delayed installation,
immediate first bootstrap worktree use, siblings/stale incarnations, and exact
cleanup. Service owns validation against the new live evidence after candidate
publication and incorporation. Do not relaunch the failed work or widen grants.

An attempted clarification of custody request 6 produced update 1 with
`UpdateNotPresented "connecting update proxy: app-server closed the connection
before responding to initialize"`. It did not incorporate the new IDs. The
existing custody obligation is unchanged. No silent retry or replacement queued
assignment was issued. The root's validation request remains separate.

## Acceptance evidence to obtain

- Exact candidate/integration revision and commands; no zero-selected success.
- Deterministic custody-delay test: bootstrap cannot use worktree before legitimate
  binding, and can use it after installation. No timing sleeps.
- Sibling/stale actor-incarnation denial and missing/failed install behavior.
- Cancellation after allocation/binding and before provider start; provider failure;
  exact release, duplicate lifecycle notices, preserved source work.
- Rebuild identity and user-owned restart for a real recursive fork canary. A source
  test or newly built executable does not update the currently running host. Never
  replace this process incidentally. Mounted canary remains separately unexecuted.

## Context-window and bookkeeping UX findings

Firsthand: obtaining a parent decision handle was not possible from this fork's
inherited bindings. `listAgents`/ActorContext exposed numeric parent identity, not
an AgentRef. Do not synthesize an opaque handle or fabricate its exit-cell custody.
Root should capture/pass its retained reference or an explicit decision callback in
future assignments; a small canonical supervisor-reference accessor needs design
and authority review if no such value exists. This is distinct from notifications.

Firsthand: `:info AgentRef` displays a representation whose nested ActorRef name is
not in public scope; broad `:browse Tidepool.Actors.Shoal` then expanded hundreds of
irrelevant declarations. Suggested small prompt/API-guide improvement: document
supported parent/peer coordination and state when no handle exists. A focused
symbol search for discovery would avoid a full-module dump; do not add another
runtime state store just for that UX.

Firsthand: the wave's typed WaveCheck/WaveDelivery retains useful evidence but has
no explicit intermediate-checkpoint discriminator. The first service delivery used
an explicit design-escalation finding/blockers, not acceptance. Consider a small
`DeliveryStage` or requested progress type for later waves; keep request settlement,
watch readiness and acceptance distinct. Prefer a progress checkpoint for a decision
while independent custody work continues. No production lifecycle semantics changed
by this document.

## Run-map owner delivery

Retained `liveEvidenceResult` is a reply from actor 2@1, request 16, at
fd6f1358d7f5b488d70f1cca6ef7caea661e6984. Its artifact is
`plans/next/run-map-custody-evidence.md` at that commit.

Reader: actor 6@1, request 7, worktree wt-ddb3c147-1550-4278-8b18-dd90a2f0a273.
Historical: actor 8@1, request 8, worktree wt-48a0d372-fe14-4430-9c79-1698c60946f9.
Both were admitted in fork group 3 from clean boundHead
936c707f55f38ac82eeeb74d63fa8c5eebc6469f with WriteForkWorktree;
supervisor/context parent 2@1, no dirty snapshot.

Both exact settlements were ReplyUnavailable / ResponseTargetFailed with:
`actor cast failed: resident workbench execution failed: turn run failed: yield
error: Haskell error: WorktreeUnauthorized (WorktreeId "<exact worktree ID above>")`.

Correction to any stronger interpretation of the original report: admission and
allocation succeeded, but the supplied evidence has no operation stack, child
command or provider-start observation. The precise bootstrap phase and whether
provider start preceded failure remain UNKNOWN. launchedProviderParent Nothing
alone does not establish either. Exact host binary hash/build revision is unknown;
the native c8460ff installation identification is inherited, not independently
rechecked for these two children. Regression validation targets the authorization
class; it cannot retrospectively establish the original timing/cause.

## Service integration checks and newly identified coverage gap

Service integrated reviewed custody candidate 0cb389a6 at
467a546701cef058ead8388136b6ea45e593e83f (staged only). Direct service reruns:

- `NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody)'`: 9 executed,
  9 passed (98 excluded), nextest d043bc0c-01bb-4262-8ca6-13d762c0d8fe.
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-actor
  'test(shutdown_intent_does_not_publish_terminal_or_replace_first_request) |
  test(shutdown_releases_mailbox_custody_deferred_behind_external_work) |
  test(independent_roots_share_routing_but_not_supervision)'`: 3 executed,
  3 passed (90 excluded).
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-handlers
  'test(actor_worktree_authority_is_exact_to_resource_and_incarnation)'`:
  1 executed, 1 passed (197 excluded); owning authority denies wrong actor,
  incarnation, resource and released binding.
- `nix develop --command cargo build -p tidepool --bin shoal`: built, NOT launched.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

Logs/identities: `target/service-custody-validation/` in service worktree
wt-2ab5550a-d87c-4d8f-8d32-69b476583c44. Shoal SHA-256
2ee9e96fbb381fccf34f3767b182278ac94d8f34ab587e6450de9e573e33949f;
host test binary 83d58b6f7066889ba6366063e3ee4722ee0dd3920501a8aefa13b31dd385e6e4.
Matched frontend/worker hashes and other test binaries are in `binaries.sha256`.
Each test wrapper observed its private compile-daemon teardown. No live host or
native process replaced.

Custody owner incorporated 467a5467 and confirmed production/test equality with
reviewed candidate. It identified an important qualification: the sibling test
awaits PolicyInstalled, marks the fork gate ready, and observes the parent unfold
commit; it does not explicitly await both SessionReady activations. The first
RunRequest's worktreeHead follows initial policy installation. Therefore these
13 passing tests do not yet directly prove success of that exact operation.
Service requested a bounded repair: require both exact SessionReady events and
exercise a nested boundHead unfold/leaf response, with independent review. This
uses private TestCampaign hosted Haskell, not native inference or observer TUI.
The strengthened evidence below closes this in-process coverage gap.

Post-submission custody deliberately remains retained: the tmux boundary cannot
prove exact process reaping. This is a product gate, not a passing cleanup path.
Do not deploy this staged candidate as complete service/custody support.


## Final strengthened regression result

Reviewed candidate 1da69df3 (test code 6028ed0a) was integrated at
**e4a5e2e8556b6346695261b83ebaabd184471c70**. Service directly ran:

```
NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody_precedes_first_bootstrap_worktree_use_for_two_siblings)'
```

One test executed and passed, 106 excluded, 29.931 seconds; nextest run
034f3af8-5469-4fa1-b797-3259254f5f49. Changed test target compiled. Both exact
sibling SessionReady requests now prove the initial production worktreeHead
finished. A real child policy performs nested boundHead unfold from a distinct
parent commit, exact leaf activation, respond/sessionInput, watch and typed reply.
Unexpected events fail; all exact root/sibling/leaf retirements, retained terminal
values, three released bindings and preserved worktree source are checked.

Direct logs: `target/service-custody-validation/exact-bootstrap.log`,
`exact-revision.txt`, `exact-binaries.sha256`. Test SHA-256
9766a6e18bcea68cd300b50440cdbcab9c285998749a85406bef11817d1ce1a5;
extractor 463d2664aea5b9e776efacd1ed7d1659735998caf340c17c676cb375401e2c93;
local worker 20ba5fbf59b459c93a78bcc31d321af104ecfd2c5af6455699ec0bee885fe323.
Formatting and diff checks passed. Compile daemon teardown was observed.
Production actor/handler/host source is unchanged from 467a5467, where the prior
negative/cancellation checks and Shoal build ran; only tests/fixtures/docs changed.
Do not describe those older runs as reruns of the strengthened test revision.

This establishes the repair's in-process recursive bootstrap behavior. It does
not establish the exact cause/timing of the two historical denials, native
full-prefix forks, native controller/observer behavior or exact OS process reap.
No running host, native pin, or live failed actor was replaced/retried. A private
hosted-Haskell canary does not need a live-root restart; exercising the fix in the
actual live Shoal process requires a rebuilt matched host/extractor/worker and a
user-owned restart. The staged irreversible post-submission custody fence still
blocks declaring this candidate deployable until service process-reap/release
integration is complete. Retained custody implementer/reviewer remain available.
