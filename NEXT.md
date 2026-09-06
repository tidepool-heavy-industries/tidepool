# Next Shoal wave — root restart handoff

## Start here, not with the old transcript

The user approved planning a recursive tree of workers to improve Shoal after the
numeric dogfood run. This is the authoritative next-wave plan, replacing
`plans/shoal-numeric-dogfood-iteration.md`. Planning is complete; implementation
has **not** started. Read this file as root, then dispatch each lead its linked
self-contained plan. Do not preload every specialist document into the shared
prefix. The documents remove dependence on this conversation or old live handles;
they do not change the default of exact full-prefix runtime forks.

Before implementation, inspect current source and nearest `AGENTS.md`; this handoff
records inspected facts, not a substitute for checking changed source. Scaffold
shared executable contracts and commit a clean seed before implementation forks.
Use resident Haskell `unfold`, retained typed requests, labeled watches, review /
repair loops and incremental integration. No fixed headcount target.

## Decisions already made with the user

- **B: separate execution service from TUI**, initially one native app-server
  service per actor, in that actor's managed execution mount. Shoal controls
  execution; TUI is an attachable observer. Reuse native components, not a new
  daemon framework or a patch to the accidental embedded-TUI endpoint.
- **Codex changes are an external dependency, not a Shoal fork.** Prefer existing
  native capabilities. If tweaks prove necessary, prepare a precise handoff for a
  separate, out-of-Shoal session in the Codex repository, managed by the human. Do not launch native
  implementation/review workers within this Shoal tree or edit Codex from it.
  Integrate the externally delivered revision/pin and verify the resulting pair.
- One control connection carries distinct operations: typed assignment, exact
  assignment amendment, one-way text notification, dependency wake and cancellation.
  A notification neither creates a reply obligation nor changes `sessionInput` or
  `respond`. It can wake an idle actor. Actor runtime still owns typed RPC semantics.
- Full-prefix specialists remain default. Small selected-context workers are a
  separate, explicit, gated mode, not an automatic context compression policy.
- Artifact-first run map, **no Tidepool dashboard**. UI work belongs in shoal-repl.
- Prioritize Shoal usage, lifecycle and evidence. Broad test-dependency surgery
  is optional, not a prerequisite. Keep native Codex goals disabled everywhere.
- Existing source work and branches must survive cleanup/restart. No silent retries
  of uncertain submitted input; no fallback from amendment to queued assignment.

## What the overnight run established

Numeric fixes were accepted at `95e5eed6cd04f850d135b5d0ba640767310d52c4`:
67 focused tests, 217 fixtures, and an independently executed hosted acceptance
with 17 probes at that same source. Outcome documentation: `1746e715`; latest
pre-handoff source: `e5a1842dd4d3302d6eb8c0519a678937599d9c7e`.
See `FLOATING_POINT_BUG_REPORT.md` for product acceptance and limits.

The tree produced valuable independent findings and repair rounds, not proof of
cost superiority. Ordinary typed request/reply and watch routes worked. Active
`updateRequest` used a *different broken route*: raw JSONL proxy versus a WebSocket
socket, and an assumed global daemon versus actors executing in embedded local
servers. Eleven not-presented warnings were retained. One admitted child failed
worktree authorization before provider start; source exposes a custody-readiness
gap, but exact historical causation is not proven.

Native app-server already has explicit Unix listen, remote TUI, after-call fork
boundaries and readiness machinery. **Missing: server-enforced controller/observer
ownership.** Ordinary attached clients can currently race to answer tool requests.
Moving the process without moving hosted-tool forwarding out of the TUI is not a fix.
Pre-wave reinspection confirms a native dependency for the agreed directly attached
observer TUI: responses lose connection identity before pending-callback completion,
and readiness has no caller role check. **Prepare the human-managed Codex handoff
now, not mid-wave.** The [external handoff and evidence](plans/next/service/native-control.md)
separate required ownership/TUI changes from existing capabilities. A headless-only
service could use existing Codex but would not close the agreed observer gate.
These findings constrain the architecture below.

## Tree and sequencing

```text
root: shared contract, manifests/pin integration, final product acceptance
├── service TL                         [first parallel wave]
│   ├── external Codex dependency (human-managed; prepare before wave)
│   ├── custody-before-bootstrap + independent review
│   └── persistent client/host bridge; then mounted integration & repair
├── run-map TL                         [first parallel wave]
│   ├── existing-artifact reader/fixtures
│   └── independent historical reconciliation/review
├── usage/evidence TL                  [first parallel wave]
│   ├── small reusable Haskell evidence helpers/consumer
│   └── usage recipes, measurement and independent review
├── fresh integrated acceptance        [after coherent candidates]
└── small-worker TL                    [only after service acceptance gate]
    ├── bounded read-only vertical slice
    └── authority/cancellation/quality review
```

| Dispatch | Exact assignment document | Exclusive lead ownership |
|---|---|---|
| Service TL | [service/ROOT.md](plans/next/service/ROOT.md) | backend control, actor lifecycle/notification semantics, host wiring; delegates custody; hands required native changes outside Shoal |
| Run-map TL | [run-map.md](plans/next/run-map.md) | derived artifact reader/report and its tests; requests instrumentation from service owner |
| Usage TL | [usage-evidence.md](plans/next/usage-evidence.md) | reusable Haskell evidence helpers, examples and usage guidance |
| Fresh acceptance | [acceptance.md](plans/next/acceptance.md) | independent integrated checks; no competing production edits |
| Gated small-worker TL | [small-workers.md](plans/next/small-workers.md) | explicit selected-context read-only slice; no second runtime |

Root owns shared manifests, dependency pin updates and cross-lead integration.
Service TL alone integrates `actor_host.rs`; custody worker provides changes there
through that TL. Run-map must not edit launch/control owners concurrently. Usage
helpers must not create new runtime response states or registries. Test files need
an explicit single owner or separate modules: logical independence did not prevent
append conflicts last time.

### First actions after restart

1. Check whether the human-managed external Codex handoff has returned; retain its
   revision/contract or gate attached-TUI integration while independent work proceeds.
   Verify workspace/branch/cleanliness and installed host, extractor, worker and
   pinned native executable identities. A restart alone does not build changed
   binaries. `just shoal-init` is the repository launcher/build entry; inspect its
   current help/script before choosing a new session. The user owns restarting
   this live root. Never replace a running host in place as an incidental test.
2. Read root guidance, current owners and the service contract sections needed to
   scaffold. Commit small real types/signatures and a representative consumer:
   actor/incarnation-bound control identity; separate notification/amendment intent;
   submission-versus-presentation evidence; revision/check delivery. Mark unsupported
   behavior explicitly, not mock success. Rust owns mechanics; Haskell stays small.
   The service TL assesses native capabilities and proposes any missing wire contract;
   required Codex changes go to an external session. Cross-lead semantics are root-owned. Do not build a general campaign framework.
3. Admit the first three TLs against that exact clean `projectHead`. Assign one
   integration owner per shared file. End the admitting tool block promptly.
4. Leads scaffold their narrower interfaces, fork implementation/test obligations,
   then fresh reviewers against exact candidates. Reviewers request repairs from
   retained implementers and keep their own review obligations pending.
5. Fold candidates independently with tested-revision evidence. Deliver accepted
   baselines back to leads and obtain incorporation/checks where relevant. Run the
   fresh acceptance branch against the *integrated* Tidepool/native binary pair.
6. Proceed to small workers only after that gate. Stop at a decision checkpoint if
   implementing the gate requires broader native session semantics than scoped.

## Evidence contract for every fold

Report candidate commit(s), integration base, exact tested revision/binary identity,
command, outcome (`ExecutedPassed`, `ExecutedFailed`, `CompileOnly`, `DidNotExecute`),
expectation (passing vs reproducing known failure), and retained evidence path.
Separate source review, attributed test runs and independent reruns. Acceptance,
integration, delivery, incorporation and resulting verification are different facts.
Never mark a compile-only or zero-selected test as behavioral success.

The decisive gate is a real mounted service with no TUI performing hosted Haskell
calls, safe TUI attach/detach, exact-boundary sibling forks, correlated amendment
and notification behavior, reconnect uncertainty, and cleanup. JSONL shell mocks
alone cannot close it. See acceptance branch for failure-path matrix.

## Restart/cleanup state

On 2026-09-06 the root executed inspected `planCleanup` / `executeCleanup` for
old technical group 1 (26 actors), ingress group 6 (3), and research group 24 (1).
All three returned `cleanupReceiptComplete = True`; actor 9 was already stopped,
other selected actors were stopped and group metadata retired. Worktrees, branches,
commits and files were not deleted. Do not try to message these old handles.
This is lifecycle receipt evidence, not a process-by-process OS leak audit.
A subsequent `listAgents` showed only `shoal-root`, with no current requests.
Root is not retired; user will restart it. Arbitrary resident values are not a
restart archive. Relevant discoveries are in these files and the artifact index.

[Overnight artifact index and measured limits](plans/next/overnight-evidence.md)
is available on demand; raw provider captures stay local/private. No runtime fix
or service canary was executed during this planning handoff.
