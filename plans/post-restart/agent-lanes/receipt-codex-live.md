# Receipt — codex-live (PRD 18 lanes 2–5: the live tool-dispatch vertical)

Lane: `plans/post-restart/agent-lanes/lane-codex-live-plan.md`. Branch:
`root.codex-live`. Contract frozen at `6df320d4` before any child forked.

## What landed

A real Codex child, spawned by `spawnAgent` into its coupled managed worktree,
holding dynamic tools that `compileTools` derived from an authored Haskell
tools record, calling those tools mid-turn, and having each call answered by
the PARENT's own Haskell handler — with activity surfaced, usage captured, the
session collected, and the worktree resolved.

## The design decision, and how it was reached

The dispatch loop lives in **Haskell**, and the Rust seam is a STEP function.
`Tidepool.Agent.Contract.Tool` carries `handler :: input -> m output` in the
parent's `M`, so a parent tool handler IS authored Haskell.

Reentrancy was CHECKED before choosing, not assumed:
`EffectHandler::handle` receives no machine handle, the machine is already
`&mut`-borrowed by `drive_effect_loop` at the dispatch site
(`jit_machine.rs:4079`), and a Haskell closure cannot reach a handler as data
(`heap_bridge.rs`'s `ClosurePolicy` rejects `TAG_CLOSURE` or substitutes
`CLOSURE_SENTINEL`; `call_closure`/`apply_cont_heap` are private). The
alternative shapes — an interposed suspending effect (the `Fork`/`AskUser`
route) or Rust-registered handlers — were rejected for reasons recorded in the
plan's §2.

Inverting the loop needs none of that machinery: each `agentBeginRaw` /
`agentResumeRaw` returns normally and the handler runs between two of them. The
thing that parks is the CHILD's JSON-RPC request, where parking costs nothing
but an unwritten response.

Per the lane-1 handoff's explicit instruction, `OneCycleBackend` was **replaced,
not widened**. `spawn_one_cycle` and `run_turn_to_completion` survive as
COMBINATORS over the step surface, so the no-tools path cannot drift from the
tools path.

## The live acceptance

`tidepool-handlers/examples/live_tool_loop.rs` — an example, not a test:
no runner can select it, no battery tier reaches it. Double-gated on a
credential existing AND `TIDEPOOL_AGENT_LIVE=1`.

**Three invocations total. Two of them spent zero model tokens.**

| # | outcome | model tokens |
|---|---|---|
| 1 | FAILED at request validation — sum-typed result → root `oneOf` | **0** (zero `thread/tokenUsage` frames; 400 before inference) |
| 2 | PASS | in 13327 (cached 13056) / out 48 / total 13375 |
| 3 | PASS — re-run after adding worktree-resolution reporting | in 13336 (cached 12032) / out 57 (reasoning 10) / total 13393 |

Model: **`gpt-5.6-luna`**, effort **low** — read off the receipt and confirmed
on the wire (`turn/start model=gpt-5.6-luna effort=low`, transcript frame 11).
`gpt-5.6-terra` is unreachable by construction, not by a skip-branch.

Run 3's receipts, from `live-tool-loop.jsonl` (105 frames, committed):

- **2 tool calls, both answered by parent Haskell handlers:**
  - `ask_parent` (a `Call`) — args `{"questionText": "Please provide the secret
    passphrase needed to finish."}`, answered
    `success:true {"passphrase":"tidepool-cobalt-7-do-not-reuse"}`.
  - `report_progress` (a `Notify`) — answered `success:true null` (the unit
    encoding).
- **`read_budget` was declared and NOT called** — the model SELECTED among
  three tools rather than calling the only one available. That is the
  difference between evidence of delegation and evidence of a funnel.
- **The Call round trip closed:** the returned `secret` is the passphrase that
  existed only inside the parent's handler. The child could not have guessed it.
- Correlation triple present on every call (thread + turn + callId).
- `rounds = 2`; `activity = []` (the task forbade commands and edits, so an
  empty activity vector is the correct observation, not a gap).
- Sandbox `writableRoots` = the bound worktree ONLY.
- **Worktree resolution:** binding settled (no Active binding remains, so the
  worktree is rebindable), worktree RETAINED at its path, head
  `666fe7f4…`, working tree clean — nothing to roll forward.
- **Config isolation held.** `config.toml`, `auth.json`, `installation_id`
  sha256-identical before and after; no new top-level files; `config.toml` text
  byte-identical. A run that succeeded while mutating `~/.codex` would be a
  FAILURE, and is checked as one.
- Wall clock 7.4s.

**No credential is handled by this code.** The app-server reads the operator's
own `~/.codex/auth.json` itself; nothing here reads, copies, or logs it, and no
secret appears in any commit, log, or receipt. The passphrase in the fixture is
a test string invented for this run.

## The finding — `outputSchema` is not arbitrary JSON Schema

The first live attempt failed:

```
invalid_json_schema: Invalid schema for response_format 'codex_output_schema':
In context=(), 'oneOf' is not permitted.
```

`outputSchema` is forwarded to the model API's structured-output
`response_format`, whose ROOT must be an object — so a sum-typed result, which
`JsonSchema` renders as a root `oneOf`, is refused. The CLI's own doc comment
calls it "arbitrary JSON Schema"; it is not.

This is the class of thing only a live child finds, and it was cheap to find:
request validation, zero tokens, an error naming `oneOf` verbatim.

Fixed where it actually misleads — **`Spawn.hs`'s haddock used a sum as THE
worked example**, so anyone following the docs would have hit it. Now a record
with `blocked :: Maybe Text`. `PROTOCOL-NOTES.md` §5 records the observation,
the consequences, and explicitly what is NOT established: whether a NESTED
`oneOf` is permitted is unknown (the message says `context=()`), and a local
precheck refusing root-`oneOf` would generalize one backend's rule from n=1, so
it is left as a design item rather than built.

Tool INPUT schemas are unaffected — a different field with a different
validator, and all three were accepted in the same run.

Second time the same lesson has bitten: **treat the generated schema and the
CLI's doc comments as a lower bound on the protocol, never as complete.** The
first was `dynamicTools` appearing absent because schema generation drops
`#[experimental]`-gated fields.

## Mock policy (root/human steering, 2026-08-11) — honored

- `MockBackend` gained a `TurnEvent` script because the SEAM's shape changed,
  and nothing else. No JSON-RPC, no frame ordering, no session lifecycle, no
  error shapes.
- Protocol behavior is proven by the REAL `Session` pump over RECORDED frames
  (`backend::codex::replay`, 11 gates, fast tier, no process, no tokens),
  originally over `phase4-live-turn.jsonl` and now also over this lane's own
  `live-tool-loop.jsonl` — one bounded live spend buying repeatable coverage.
- Hand-scripted fixtures remain only as arrange-step input.

The `replay-transport` child DECLINED to drive the full `CodexAgentBackend`
over the phase-4 recording, because `start_turn` issues `model/list` and that
recording contains no such exchange — replaying it would mean hand-writing the
one invented frame the policy exists to forbid. Correct refusal; the gap is
named in PROTOCOL-NOTES.md rather than papered over. **This lane's own
recording contains `model/list`, so that gap is now closeable.**

## The mode seam — no second interpretation emerged

Standing condition: document a second `Call`/`Notify`/`AsServerT` mode
interpretation **if one emerges naturally**, never invent one speculatively.

None emerged. This lane INTERPRETED the existing `AsServerT` server mode and
nothing else; `Contract.hs` is byte-identical across the whole lane. The
seam's shape was sufficient for a live child without modification, which is
itself the result worth recording — reported as "no note", not as a gap.

## Gates (all by name; base commits stated)

`tidepool-agent` — 63 run / 63 passed / 3 skipped (the pre-existing `#[ignore]`d
live tests), `scripts/battery.sh -p tidepool-agent -E 'all()'`, at `39fccf2c`.
Includes the 11 `backend::codex::replay::tests` protocol gates, among them
`start_turn_parks_at_the_recorded_tool_call_having_written_no_response` (the
one a mock cannot check — it asserts the pump wrote NOTHING back) and
`a_reply_to_the_wrong_call_is_refused_without_losing_the_parked_call`.

`tidepool-handlers` — 12 selected / 12 completed / 12 passed, at the
`haskell-loop` merge, via `-E 'binary(subagent_tool_loop) + binary(subagent_one_cycle)'`:

- `call_answer_from_the_parent_handler_reaches_the_child`
- `notify_dispatches_and_answers_with_the_unit_encoding`
- `parent_effect_runs_while_the_child_is_parked`
- `undeclared_tool_is_refused_and_the_loop_continues`
- `past_the_round_cap_calls_are_refused_and_the_turn_still_completes`
- `a_tools_compile_error_returns_before_anything_is_spawned`
- `declared_tools_reach_the_backend_with_wire_names_and_schemas`
- plus lane 1's 5 `subagent_one_cycle` gates, unbroken.

Full-crate `tidepool-handlers` 172/172 and `tidepool-runtime`
`binary(agent_mode_encoding)` 14/14 were run by the `haskell-loop` child at
`e7ce7e7c`.

Instrument for every count above: the `cargo-nextest` summary line.

**Zero live model calls in any automated suite.** The only live calls in this
lane are the three deliberate example invocations tabulated above.
