# Lane plan — codex-live: the live tool-dispatch vertical (PRD 18 lanes 2–5)

TL: `root.codex-live`. Picks up
[`lane1-to-lanes2-5-handoff.md`](lane1-to-lanes2-5-handoff.md). Written BEFORE
any code, per the lane's step 1.

**What this lane delivers:** a real Codex child, spawned by `spawnAgent` into
its coupled managed worktree, holding dynamic tools that `compileTools` derived
from an authored Haskell tools record, calling those tools mid-turn, and having
each call answered by the PARENT's own Haskell handler — with activity
surfaced, the session collected, and one deliberate live run on the human's
granted budget.

---

## 1. What "lanes 2–5" mean against today's tree

The handoff's lane numbering was written at lane 1's fold. Two things have
moved under it since, and one instruction from the human has re-pointed it.

**Moved:** (a) the two committed codecs are GONE — `Agent.CodecSpike` and
`Agent.ModelCodec` were deleted and the model boundary is now plain vendored
`ToJSON`/`FromJSON` plus `Tidepool.Aeson.Schema.JsonSchema` (settled
2026-08-10; `haskell/CLAUDE.md`'s `Agent/*` entry is the current statement).
So the handoff's whole "keep it two codecs" section, and its routing note that
`compileTools`' input side should consume `ModelCodec`'s field machinery, are
both dead letters: `compileTools` already reads `JsonSchema`/`FromJSON`
directly (`Tidepool/Agent/Contract.hs`), which is the same single-traversal
property the note wanted, reached by deletion instead of by plumbing.
(b) the forms rework landed, so `Tidepool.Form`'s row-gating shape — the
precedent `Tidepool.Agent.Spawn` is written against — is the reworked one.

**Re-pointed:** this lane's mandate is not "build lanes 2, 3, 4 and 5 as
chartered". It is ONE vertical: the live dispatch loop. So the charter lanes
map onto it as follows.

| Charter lane | Status in this lane |
|---|---|
| 2 — coupled-spawn failure/saga matrix | **Extended, not built.** The saga grows stages (turn started, parked on a call, round cap reached), so the existing stage vocabulary gains rows and the existing rollback rule is re-applied to them. A standalone failure MATRIX is still deferred. |
| 3 — durable poke/interrupt ordering | **Not built.** But this lane is the first that ever PARKS, so it is the first that *could* answer spike 2's open question (does a parked `item/tool/call` block `turn/interrupt`). Measured opportunistically if the live run affords it; never designed past. |
| 4 — cross-cycle detach/reattach + tool-handler wakeup | **The tool-handler half is THIS LANE.** "Reattach supplies tools" reduces, inside one cycle, to: declarations at thread start, dispatch on each parked call. Detach/reattach across cycles stays deferred (it needs the durable `AgentId` mint the handoff routes). |
| 5 — mailbox/staleness | **Not built.** |

The handoff's central instruction is honored literally: **`OneCycleBackend` is
REPLACED at the seam, not widened.** Its `start_thread` + `run_cycle` shape
cannot express a parked turn, and this lane's whole subject is a parked turn.

---

## 2. The one design decision: where the dispatch loop lives

`Tidepool.Agent.Contract.Tool` carries `handler :: input -> m output`, and
`compileTools` yields `dispatch :: ToolName -> Value -> m Value` — in the
PARENT's `M`. That is the authored surface the human's design already froze,
and it is not negotiable-around: a parent tool handler is Haskell that performs
parent effects.

So the loop is in **Haskell**, and the Rust seam becomes a STEP function:

```text
Haskell                                   Rust                    codex app-server
spawnAgentWithTools tools spec
  compileTools tools ────────────────► declarations
  agentBeginRaw spec decls schema ───► saga: worktree, binding,
                                       thread/start{dynamicTools},
                                       turn/start, pump frames
                                                    ◄────────── item/tool/call  (PARKED)
  ◄── StepToolCall callId tool args
  dispatch tool args   (parent effects run here — ordinary M)
  agentResumeRaw callId reply ───────► write the parked JSON-RPC response,
                                       pump on
                                                    ◄────────── turn/completed
  ◄── StepDone outcome
  decode outcome payload as `r`
```

The parked `item/tool/call` is simply a JSON-RPC request whose response has not
been written yet. Nothing in the app-server needs to know the parent went away
to run Haskell. This requires no JIT reentrancy, no continuation parking, and
no async handler seam — each individual effect call stays synchronous and
run-to-completion, which is the property the whole handler layer is built on.

**Alternatives rejected, against a checked mechanism survey** (not assumed —
the machine was read before choosing):

- *Re-enter the JIT from inside the Rust handler.* **Impossible, on two
  independent grounds.** `EffectHandler::handle` receives only `&mut self`, a
  decoded request, and an `EffectContext` exposing `table()`/`user()` — no
  machine, no vmctx (`tidepool-effect/src/dispatch.rs`); and the machine is
  already `&mut`-borrowed by `drive_effect_loop` at the dispatch site
  (`tidepool-codegen/src/jit_machine.rs:4079`), so reentrancy is not even
  expressible. Separately, a Haskell closure cannot arrive as handler data at
  all: `heap_bridge.rs`'s `ClosurePolicy` rejects `TAG_CLOSURE` outright or
  substitutes `CLOSURE_SENTINEL`, and the primitives that *do* apply a closure
  (`call_closure`, `apply_cont_heap`) are private to the machine.
- *Make the tool call an INTERPOSED effect that suspends to a driver* — the
  `Fork`/`AskUser` route, which is how Haskell services a request today. It
  works, but it drags in the continuation-parking contract's invariants
  (exact-prefix equality, no mixing slot and registry paths, cycle-scoped realm
  lifetime) for no gain here, and it would make `spawnAgentWithTools`
  un-callable from an ordinary eval that has no such driver.
- *Answer tool calls from Rust-registered handlers inside `run_cycle`.* Keeps
  the seam narrow but throws away the entire point of `compileTools` — the
  handlers would no longer be the authored Haskell ones, and the
  declaration/dispatch single-traversal guarantee would span two languages with
  nothing tying them together.

**Why the chosen shape needs none of that machinery:** inverting the loop into
Haskell means the Haskell side is never suspended when a parent handler runs.
Each `agentBeginRaw`/`agentResumeRaw` is an ordinary synchronous effect call
that returns normally; the parent handler runs between two such calls, as plain
Haskell in `M`. What is parked is the *child's* JSON-RPC request, on the far
side of the seam, where parking costs nothing but an unwritten response.

**Consequence to write down:** the app-server turn is parked for as long as the
parent's Haskell handler runs. If the parent eval dies mid-dispatch, the call
is never answered and the turn hangs until the child's own timeout. The
mitigation is ownership, not a protocol trick: `SubagentHandler` owns the
backend, and dropping it kills the app-server process. Stated in the handler's
docs; not hidden.

---

## 3. Mock policy (root/human steering, 2026-08-11) — pinned

Binding on every child in this lane:

1. **`MockBackend` stays a DUMB seam implementation.** Scripted answers to
   seam-trait calls; zero Codex protocol semantics — no JSON-RPC, no frame
   ordering, no correlation bookkeeping, no session lifecycle. By the fold it
   must be no smarter than it is today. It grows exactly one thing: the script
   becomes a list of `TurnEvent`s instead of a single payload, because the
   seam's own shape changed. That is not protocol knowledge.
2. **Protocol behavior is proven by the REAL adapter over a RECORDED
   transcript.** A new `backend::codex::replay` transport feeds the real
   `Session` pump real recorded JSONL frames, so frame ordering, the parked
   correlation triple, the `success:false` shape and `turn/completed`
   projection all run through production code with zero live calls. The
   existing `fixtures/app-server-0.146.0/phase4-live-turn.jsonl` (35 frames,
   one real `ask_parent` round trip) is available on day one; this lane's own
   live run records a second, multi-round transcript on luna/low.
3. **Hand-scripted fixtures stay** for deterministic harness-contract tests
   ("given exactly this reply, the loop does X") — arrange-step input, not
   simulation.

If a test can only pass by teaching the mock protocol behavior, the correct
response is to move the seam or record a transcript, never to teach the mock.

---

## 4. Contract freeze (scaffold commit) — then fork

Lane 1's strongest evidence was that freezing the contract before forking
produced two independent waves that met with zero reconciliation edits. Same
shape here: I author the frozen contract, commit it, and children fork from
that commit.

### 4a. `tidepool-agent/src/seam.rs` — new vocabulary

```rust
pub struct ToolCallId(pub String);       // opaque backend correlation token

pub struct ToolCall {                    // one parked call
    pub call: ToolCallId,
    pub thread: BackendThreadId,
    pub turn: TurnId,
    pub tool: String,
    pub arguments: serde_json::Value,
}

pub struct ToolReply {                   // what the parent answered with
    pub call: ToolCallId,
    pub outcome: ToolOutcome,            // Ok(Value) | Failed(String)
}

pub enum TurnEvent {                     // why the pump stopped
    ToolCall(ToolCall),
    Completed(CycleOutcome),
}

pub enum ReasoningEffort { Low, Medium, High }
```

`CycleSpec` gains `effort: ReasoningEffort` (the protocol accepts it at
`turn/start` and the live budget requires LOW). `CycleOutcome` keeps its
existing fields and gains `usage: Option<TokenUsage>` — the handoff routes the
`thread/tokenUsage/updated` notifications that `drive_turn` currently discards,
and this lane's receipts must report usage numbers, so this is the lane that
must pick them up.

`ModelPolicy` gains a second variant. The allowlist mechanism is preserved
exactly (that is the reason `gpt-5.6-terra` is unreachable by construction):

| policy | allowlist, in order |
|---|---|
| `CheapPlumbing` | `gpt-5.4-mini`, `gpt-5.6-luna` |
| `CheapestGpt56` | `gpt-5.6-luna` — and nothing else |

`CheapestGpt56` exists because the human's 2026-08-11 budget grant names that
exact tier at LOW effort. `CheapPlumbing` would resolve to `gpt-5.4-mini` on
the observed catalogue, which is not what was granted.

### 4b. `tidepool-agent/src/backend/mod.rs` — the replacement trait

```rust
pub trait AgentBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError>;
    fn start_turn(&mut self, thread: &BackendThreadId, spec: &CycleSpec)
        -> Result<TurnEvent, AgentBackendError>;
    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError>;
}
```

`OneCycleBackend` is deleted. `run_cycle` becomes a default-free combinator
(`start_turn` then `resume` until `Completed`) available to callers that
declare no tools — lane 1's synchronous surface survives as a combinator, which
is the PRD's own rule for synchronous delegation.

### 4c. `tidepool-mcp/src/effect_defs.rs` — the wire contract

Two new verbs on `Subagent`, and the existing `SubagentSpawn` kept (it is the
no-tools path and lane 1's committed acceptance drives it).

```haskell
data ToolDecl   = ToolDecl { declName :: Text, declDescription :: Text, declSchema :: Value }
data ToolAnswer = ToolAnswered Value | ToolRefused Text
data AgentStep  = StepToolCall { stepCallId :: Text, stepTool :: Text, stepArgs :: Value }
                | StepDone SpawnOutcome

agentBeginRaw  :: SpawnSpec -> [ToolDecl] -> Value -> M (Either SpawnError AgentStep)
agentResumeRaw :: Text -> ToolAnswer -> M (Either SpawnError AgentStep)
```

`SpawnOutcome` gains `outcomeActivity :: [AgentActivity]` and
`outcomeUsage :: Maybe TokenUsage`. Surfacing activity was explicitly deferred
to root in `handlers/agent.rs` ("Adding it is a `type_defs` change (root's
call), not a silent widening here"); this lane's mandate names activity
surfacing as a deliverable, which is that call being made.

### 4d. `haskell/lib/Tidepool/Agent/Spawn.hs` — the authored combinator

```haskell
spawnAgentWithTools ::
  forall tools r. (HasAgentApi tools M, FromJSON r, JsonSchema r) =>
  ToolRounds -> tools (AsServerT M) -> SpawnSpec ->
  M (Either SpawnError (SpawnOutcome, r))
```

`compileTools` once → declarations → `agentBeginRaw` → loop
`dispatch`/`agentResumeRaw` → decode. `spawnAgent` keeps its exact current
signature and becomes `spawnAgentWithTools` at zero tools.

**Round cap** is `ToolRounds`, enforced in the Haskell loop: past the cap the
loop answers `ToolRefused "tool-call round cap reached (N)"` instead of
dispatching, so the child finishes its turn normally rather than being
interrupted. Cap is policy, and policy is the resident's — same boundary the
README states for escalation ("the runtime exposes observations; the resident
decides"). A `ToolCompileError` is a typed `SpawnError` before anything is
allocated.

---

## 5. Waves

**Wave 0 (me, before forking):** this document; the contract freeze in 4a–4d,
committed with stubs that typecheck and `todo!()`/`error` bodies, so every
child forks from one commit that already agrees with itself.

**Wave 1 (fork, parallel):**

| child | owns | proves |
|---|---|---|
| `adapter-step` | `tidepool-agent/src/backend/codex/` — split `drive_turn` into a resumable pump, park/reply, usage capture, effort at turn start, `CheapestGpt56` | the REAL adapter, driven by the replay transport over `phase4-live-turn.jsonl`, parks and resumes a real recorded tool call |
| `replay-transport` | `tidepool-agent/src/backend/codex/replay.rs` + the transport abstraction under `RawAsyncClient` | a recorded transcript drives the production `Session` with zero live calls |
| `seam-and-mock` | `seam.rs`, `backend/mod.rs`, `backend/mock.rs`, `spawn.rs` saga extension + `tests/spawn_saga.rs` rows | the saga rolls back correctly from every new stage; MockBackend stays dumb |

**Wave 2 (fork after wave 1 folds):**

| child | owns | proves |
|---|---|---|
| `handler-verbs` | `tidepool-handlers/src/handlers/agent.rs` + effect_defs verbs | wire↔domain totality for the two new verbs, session state machine (resume without begin is a typed error) |
| `haskell-loop` | `Tidepool/Agent/Spawn.hs` + `tidepool-handlers/tests/subagent_tool_loop.rs` | the WHOLE loop on the real extract/JIT against MockBackend: N rounds, a Notify, the round cap, a parent-handler effect actually running |

**Wave 3 (me):** the live acceptance, docs, submit.

---

## 6. The live acceptance

ONE run. `tidepool-handlers/examples/live_tool_loop.rs` — an example, not a
test, so no runner can select it; double-gated on `~/.codex/auth.json` existing
AND `TIDEPOOL_AGENT_LIVE=1`.

Credentials: **none are handled by this code, deliberately.** The codex
app-server reads the operator's own `~/.codex/auth.json` itself; Tidepool never
reads, copies, or logs it, and `tidepool_runtime::paths`' secrets layer is not
involved because there is no Tidepool-held credential to resolve. Isolation
across the run is proven by the existing `ConfigSnapshot` checker.
`~/.codex/auth.json` is present on this box, so the acceptance is not
blocked-on-credential.

Shape: a real temp source repository → `spawnAgentWithTools` with **three**
declared tools —

- `askParent :: Call Question Answer` (the Call round trip),
- `reportProgress :: Notify Progress` (the Notify),
- `readBudget :: Call BudgetQuery Budget` (a second Call, declared but not
  required, so the transcript shows selection among tools rather than the only
  possible action)

— on `CheapestGpt56` + `ReasoningEffort::Low`, round cap 4, a task written to
need exactly one `askParent` and one `reportProgress`. Then: collect the
session (outcome, receipt, activity, usage), print the receipt, and resolve the
worktree — report its head and whether the child committed, leaving it retained
and UNBOUND per retain-first.

The run records its full JSONL transcript into
`fixtures/app-server-0.146.0/live-tool-loop.jsonl`, which becomes the replay
fixture for a permanent multi-round CI test. That is the record/replay half of
the mock policy: one bounded live spend buys forever-repeatable protocol
coverage.

Receipts reported at submit: model slug (exact), effort, rounds observed, tool
calls with their correlation triples, park durations, token usage from
`thread/tokenUsage/updated`, wall clock, and the isolation report.

**One attempt after `turn/start`**, per the standing rule. Failure → capture
the frame log, report, stop. Free-to-retry dry runs (everything up to but not
including `turn/start`) are unlimited and are how the run is de-risked first.

---

## 7. Non-goals (explicit)

Pokes/steer/interrupt; `waitAgent`/async handles; detach/reattach and durable
`AgentId`; the mailbox; multi-agent concurrency; any change to the
`Call`/`Notify`/`AsServerT` mode seam's SHAPE (this lane INTERPRETS it — a
second interpretation gets documented only if one genuinely emerges); the
consolidation surfaces siblings just landed (Aeson generics, FormShape,
ExtractCmd, the compile memo) beyond consuming them; fixture regeneration under
`haskell/test/{suite_cbor,corpus_cbor}`.

---

## 8. Verification tiers for this lane

- `cargo check --workspace --all-targets`, `cargo clippy --workspace
  --all-targets`, `cargo fmt --all -- --check`
- `cargo nextest run` — quick tier, zero live calls
- `scripts/battery.sh -p tidepool-agent -E 'all()'` — seam, saga, replay
- `scripts/battery.sh -p tidepool-handlers -E 'all()'` — the loop on the real JIT
- `scripts/battery.sh -p tidepool-runtime -E 'binary(agent_mode_encoding)'` —
  `compileTools` still green where it already was
- the live example, run once, by hand, reported

Named pass lines for every guard that exists to catch one failure mode, with
the base commit — the directory's receipt rule.
