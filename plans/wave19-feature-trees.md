# Wave19: four parallel feature trees

Status: proposed product scope, prepared before wave18 completion. No launch yet.
Source examined: wave18 root ecf7a2616840ae8f2bbd864b59cfce630945d720;
accepted-but-unmerged wave18 work and its final acceptance must be incorporated
before freezing the wave19 baseline. User requests a Sol root with 3–5 mostly
independent feature areas, each naturally owned by a Luna subtree.

## Milestone

Make the standalone deterministic harness easier to inspect, replay and operate
under asynchronous load. Four feature owners deliver reviewed components. Root
integrates one browser-operable result. No credentialed inference, Exomonad
adapter, replacement scheduler or additional durable journal.

## A. Durable node and request inspection

Gap: App.tsx currently exposes tree/activity/inbox/command views; the server
snapshot contains conversations, requests, jobs and envelopes, not a request
history read surface. Store already retains items and recorded model turns.

Luna lead, two implementation children, independent reviewer:

- Backend child: bounded authenticated history reads through existing Store;
  preserve chronological item/call identity and distinguish exact as-sent model
  input from subsequently appended items. Own history query module and route.
- Browser child: node/request window, deep links, pending/late/interrupted output,
  paging and explicit unavailable evidence. Own new NodeWindow and client module.
- Reviewer: exact request identity, authorization, paging boundaries and reopen.
- Lead integrates focused service/browser tests and returns one accepted candidate.

Primary paths: server/history projection, new web NodeWindow files. Root owns
shared App navigation and protocol declarations. Acceptance: a real deterministic
root/child request can be deep-linked and inspected before/after reopening the
same Store, with no invented as-sent history or silent empty-success result.

## B. Portable, faithful offline replay

Gap: ReplayProvider::new requires Arc<Store>; request_mismatch currently omits
request restrictions, and ReplayProvider inherits the pass-through before-request
hook rather than reproducing recorded restriction/injection decisions.

Luna lead, two implementation children, independent reviewer:

- Fixture child: versioned export/import of a selected recorded branch, exact
  model requests/responses, call outputs and necessary hook evidence/provenance.
  Reuse Store reads; introduce no second recording owner.
- Replay child: faithful recorded decisions and complete request comparison;
  mismatch must not consume the replay cursor. Own replay implementation/tests.
- Reviewer: incomplete fixture, wrong branch/call identity, altered restriction,
  format rejection and offline/no-provider behavior.
- Lead integrates an executable export/replay demonstration and evidence.

Primary paths: replay.rs and adjacent fixture module/tests. Root owns any CLI
registration. Acceptance: a deterministic recording replays from a fresh process
and isolated imported data, without the original DB or credentials. Known mismatch
is explicit and a corrected request can still consume the same recorded turn.
Exports can contain conversation content: do not promise arbitrary fixtures are
secret-free or apply redaction that silently breaks exact replay. The demo uses
synthetic non-sensitive data and excludes transport credentials.

## C. Bounded provider progress and cooperative cancellation

Gap: CallContext exposes an UnboundedSender; JobScheduler drains it into an
unbounded Vec. Cancellation currently aborts a task rather than exposing a
provider cancellation signal. Existing scheduler remains the sole terminal owner.

Luna lead, two implementation children, independent reviewer:

- Progress child: bounded producer and retained observation paths; explicit
  overflow/retention semantics, visible omitted-progress evidence and consumer
  migration. Terminal results are distinct from lossy progress.
- Cancellation child: provider-visible cancellation and terminal-race handling;
  preserve cancellation before admission and first-terminal-wins. Do not claim a
  cancellation request proves external cleanup finished.
- Reviewer: saturated progress, slow consumer, cancel/start/settle races, late
  emissions, and an uncooperative provider. Use explicit barriers, not sleeps.
- Lead integrates provider/cell consumers and one deterministic acceptance case.

Primary paths: provider.rs, turn.rs, cell_job.rs and their tests. Root scaffolds
CallContext types and decides bounded-progress policy before workers fork. If
cooperative cleanup needs a grace policy, make it explicit and bounded; do not
invent an unbounded shutdown wait. Acceptance: progress flood has bounded memory,
cancellation reaches a cooperative provider, and exactly one terminal outcome
survives without late resurrection. Engine changes require root-owned integration.

## D. Time-based tree and timeline

Gap: the web brief calls for branch/time lanes; App currently renders a recursive
table and activity rows, and its view model lacks the timing needed for spans.

Luna lead, two implementation children, independent reviewer:

- Projection child: source-backed start/end observations for request/job spans,
  fork relationships and open/unknown ends. Extend the existing snapshot/event
  projection, never reconstruct duration from arrival time in the browser.
- Visual child: dedicated TreeTimeline/layout components, selection and keyboard
  navigation plus an equivalent readable table; keep unknown time visible.
- Reviewer: deterministic fixture, overlapping spans, reconnect, missing timing,
  keyboard/200%-zoom/reduced-motion behavior and a larger tree.
- Lead integrates the production projection with the view and acceptance evidence.

Primary paths: new demo observation/projection module and new web timeline files.
Root owns App navigation, shared protocol/schema and dependencies. Acceptance:
actual deterministic async activity produces branch/request/job spans, open work
has an honest open end, and selection survives reload and identifies the same
request that lane A can inspect. A fixture-only pretty picture does not pass.

## Scaffold and ownership before parallel work

Sol fixes the accepted source baseline and a small shared example first. Root owns:

1. Store-backed read-service attachment to server, route and request identity.
2. Shared browser schema and navigation slots; typed bindings have one owner.
3. Replay fixture/version and hook-decision association contract.
4. CallContext progress/cancellation contract and terminal authority.
5. Thin main.rs composition, manifests/lockfiles and final acceptance journey.

Do not have four leads edit App.tsx, main.rs or engine.rs independently. Supply
new modules and small integration slots. A owns necessary history Store reads;
B consumes existing/frozen Store APIs or requests a root seam change. D owns the
observation projection; C owns scheduler/provider mechanics. Freeze shared choices
before dependent forks, and deliver later corrections only to affected owners.

Each Luna lead delegates meaningful local obligations, integrates its children,
commissions independent review and returns a checked candidate. Root gets four
component deliveries, not a stack of leaf patches. Model concurrency is useful;
expensive builds remain bounded by actual memory. Require no headcount ceremony
or invented work just to fill a tree.

## Bounded orchestration experiment

Use an existing collector/record actor and the tested pattern helpers to handle
one recurring integration-update workflow. Keep original responses and exact
source/check evidence. Compare new updates with explicitly incorporated facts;
uncertain or materially changed updates return to the owner. First evaluate in
shadow alongside normal delivery. Do not add automatic notice suppression or
merge authority. A helper should replace a concrete repeated sequence, not add
another report the root must interpret.

Retain examples of decisions, wrong branches, handbacks, input tokens and actual
frontier turns avoided. Each lead identifies a useful reusable check/evidence
composition and passes it to its children when worthwhile; adoption is not a quota.

## Launch gates

Wave18 product acceptance/interviews and deliberate resource disposition; reviewed
inter-wave integration; focused combined checks; matched binary/workspace pin and
prompt revisions. Keep the next wave Sol-root. Launch this brief only after the
user's product-scope discussion and required gates, without overlapping waves.
