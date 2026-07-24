# tidepool-harness — typed-yield session harness (R0 build)

A new frontend over the eval substrate — NOT a retrofit of tidepool-repl
(which keeps its parked-thread mechanism unchanged). Plan + segment specs:
`plans/harness-r0/`; cross-segment contracts:
`plans/harness-r0/00-scaffold/contracts.md`; frozen shapes:
`plans/harness-r0/FREEZES.md`.

Module map:
- `tree`/`forcing` — `NodeId`/`NodeState`/`HoleId`/`SiteId`, forcing badges,
  and `NodeTree<M>`: parent/child structure + per-node lifecycle state,
  backed by the durable event log. Generic over a machine handle `M` and
  backed internally by a `SessionRegistry<M>` (see Machine lifecycle below).
- `registry` — `SessionRegistry<M>`: the `Idle | Running | RunningChild |
  Suspended` slot machine + atomic checkout/restore, including nested-child
  checkout (segment 40, landed — `checkout_child`/`RunningChild`).
- `harness` — `Harness`: the orchestrator. Owns a `NodeTree<()>` for tree
  bookkeeping and a separate `convos` map holding the real resident sessions
  (see Machine lifecycle below); drives the turn loop, hole classification,
  fork/fanout registration, elaborator proposal confirm/reject (B2).
- `engine` — the turn engine: prompt assembly, provider call, extract+compile
  the last fenced Haskell block, classify an `AskWith` suspension.
- `compile` — turn compilation (Haskell source → `CoreExpr` + `DataConTable`
  + `asks.json` sidecar) via `tidepool-extract`, independent of
  `tidepool-runtime`'s caching compile (turns are one-shot, no cache needed).
- `log` — E4 event-log wire schema (header pins prelude+extract fingerprints);
  `Event::Effect` (req+resp) is a reserved wire slot — see Replay below for
  what's actually written today.
- `provider` — `ModelProvider` trait (calling-model turns; not the Llm
  effect) + `provider/{api_key,http,oauth,paths}` impls.
- `replay` — `ReplayProvider` (turn substitution) + `fold_tree_state`
  (crash-replay tree reconstruction) — see Replay below.
- `ui`/`uiof` — the `Ui` eDSL wire mirror (Haskell `Tidepool.Ui`'s contract
  partner) and `uiOf` (server-derived mechanical forms from a compiled
  `DataConTable`, no Haskell Generic machinery).

## Machine lifecycle — `convos`, not the registry, on the harness's live path

`SessionRegistry<M>`'s checkout/restore discipline is fully built and tested
(`registry.rs`), but it is UNUSED ON THE HARNESS'S LIVE PATH: `Harness`
instantiates its `tree` field as `NodeTree<()>` (`M = ()`), so that internal
registry never holds a live session. This is not dead code to delete — its
removal, if ever warranted, is a separate follow-up, and the design it
embodies may still be worth preserving. Do not claim "all machine access
goes through the registry" — it doesn't, today.

The real resident sessions live in `Harness::convos: Mutex<HashMap<NodeId,
NodeConvo>>`, where `NodeConvo.session: Option<Session>` holds the actual
handle. `Harness::take_session`/`put_session` implement their own
take-out/put-back discipline directly against `convos` (mirroring the
stowed-XOR-running shape by hand: `take_session` moves the session out for
the turn's duration, every public method that could observe the gap holds
the `convos` lock across the take, `put_session` restores it after). The
`NodeTree<()>` is the tree/state/log bookkeeping half of the design (node
creation, forcing, hole publication, turn/done/cancel logging — but NOT
effect logging, see Replay below) — a real mechanism, just not the one
gating machine access.

## Replay — turn substitution + crash-replay tree reconstruction, NOT effect replay

Two independent pieces, both in `replay.rs`:

- **Turn substitution** (`ReplayProvider`): a `ModelProvider` that serves
  previously-recorded assistant `TurnDelta` replies back in order instead of
  calling a live model — a CI run re-drives the same golden path with zero
  API calls.
- **Crash-replay** (`fold_tree_state`): folds a log's events into the
  terminal per-node `NodeState` + tree structure, so a `kill -9`'d process
  restores a browsable history tree from the durable log alone.

**`Event::Effect` is a reserved wire slot, not currently written or
substituted.** The req+resp shape and its writer (`NodeTree::effect`,
`forcing.rs`) exist and are exercised by a `forcing.rs` unit test, but
nothing in the live turn loop (`harness`/`engine`) calls that writer — no
`Event::Effect` records are produced during real operation today (so
`tidepool-web`'s trace pane, which folds `Event::Effect` out of the log, has
nothing to show against a live-run log). Nor is there a reader that
substitutes recorded responses back into a resumed session on restart — were
effects being logged, a node that suspended after running them, then
restarted and resumed, would RE-EXECUTE those effects live rather than
replay their recorded responses. Effect-response-substitution replay is
explicitly OUT OF R0 SCOPE; the wire slot and writer are frozen and reserved
for that future work, not deprecated.

## Rules inherited from the plan

Forcing events are the only work-begins mechanism (consent integrity audits
to literal zero — `NodeTree::force` is the only transition out of `Thunk`,
and it logs `Event::Forced{actor}` before any session exists); teasers are
harness-generated only (`forcing.rs::derive_teaser`).
