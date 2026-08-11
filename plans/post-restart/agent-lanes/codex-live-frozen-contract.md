# codex-live — the frozen contract (wave 0)

Authored by the lane TL BEFORE forking, at `ac4f17d6` (the seam replacement).
Lane 1's strongest evidence was that two independent waves written against a
frozen contract met with **zero reconciliation edits**; this is the same move.

Every child implements against the text here. Where it is silent, ask the TL —
do not decide and diverge. Where it is wrong, say so before building on it.

---

## Already landed at `ac4f17d6` (consume, do not redesign)

`tidepool-agent`:

- `seam.rs`: `ToolCallId`, `ToolCall`, `ToolOutcome`, `ToolReply`, `TurnEvent`,
  `ReasoningEffort`, `TokenUsage`; `CycleSpec.effort`; `CycleOutcome.usage`;
  `ModelPolicy::CheapestGpt56`.
- `backend/mod.rs`: `trait AgentBackend { start_thread, start_turn, resume }`
  plus the `run_turn_to_completion` combinator. `OneCycleBackend` is deleted.
- `backend/codex/driver.rs`: the real `AgentBackend` impl, effort at turn start,
  per-policy model allowlists, usage projection.
- `backend/codex/process.rs`: `Session::{start_turn, reply_and_pump}` — the
  resumable pump, holding the parked JSON-RPC id. `TurnStop` is its result.
  `drive_turn` survives as a combinator so the committed live `#[ignore]`d
  tests keep running.
- `spawn.rs`: `CoupledSpawner::{begin, answer}` → `SpawnStep`, one running agent
  at a time, `MAX_TOOL_ROUNDS` backstop, `SpawnError::{NotRunning,
  RoundBackstop}`; `SpawnRequest.{tools, model, effort}`;
  `SpawnReceipt.{rounds, usage}`.
- `backend/mock.rs`: `MockBackend::scripted([MockStep])`. **Do not make it
  smarter** (see "Mock policy" below).

`tidepool-handlers`: `SubagentHandler` holds `Box<dyn AgentBackend + Send>` plus
a configured `(ModelPolicy, ReasoningEffort)`, set with `with_model_policy`.

`tidepool-mcp`: `SpawnError` gained a `SpawnDriveFailed SpawnStage Text` ctor.

---

## The Value lane, and why the wire types are shaped this way

`serde_json::Value` has **`ToCore` but no `FromCore`**. Inbound JSON therefore
cannot ride inside a bridged record; it rides as a separate verb argument
converted by `crate::effect_glue::JsonArg` (which IS `FromCore`, decoding
against the dispatch's own `DataConTable`). `schema` already rides that lane on
`SubagentSpawn` for exactly this reason.

Consequence, and it is a real constraint, not a preference:

- **Inbound (Haskell → Rust): flat arguments.** Tool declarations arrive as ONE
  `Value` (a JSON array), and a tool answer as a `Bool` + a `Value`. Rust parses
  them with serde.
- **Outbound (Rust → Haskell): proper ADTs.** `AgentStep` and everything under
  it are ordinary bridged types with `Value` fields.

The authored surface never sees this asymmetry: `spawnAgentWithTools` builds the
array and destructures the ADT. A `ToolAnswer` ADT exists **authored-side only**
(in `Tidepool.Agent.Spawn`) — an authored type does not have to be a wire type.

---

## Frozen: `subagent_effect_def!` additions (`tidepool-mcp/src/effect_defs.rs`)

### `type_defs` — new and changed

```haskell
-- NEW
data AgentActivity = ActivityCommand Text (Maybe Int) | ActivityFileChanged Text deriving (Show, Eq)
data TokenUsage = TokenUsage { usageInput :: Int, usageCachedInput :: Int, usageOutput :: Int, usageReasoningOutput :: Int, usageTotal :: Int } deriving (Show, Eq)
data AgentStep = StepToolCall AgentId Text Text Value | StepDone SpawnOutcome deriving (Show, Eq)

-- CHANGED (append-only; field order is the wire contract, so APPEND, never insert)
data SpawnReceipt = SpawnReceipt { …existing…, receiptRounds :: Int, receiptUsage :: Maybe TokenUsage } deriving (Show, Eq)
data SpawnOutcome = SpawnOutcome { outcomeRun :: WorkerRun, outcomePayload :: CyclePayload, outcomeReceipt :: SpawnReceipt, outcomeActivity :: [AgentActivity] } deriving (Show, Eq)
```

`StepToolCall`'s fields are `AgentId`, callId, tool name, arguments — positional,
matching `CyclePayload`'s existing style for a sum with a `Value` payload.

`outcomeActivity` closes the deferral `handlers/agent.rs` recorded verbatim
("Adding it is a `type_defs` change (root's call), not a silent widening here").
This lane's mandate names activity surfacing as a deliverable; that is the call
being made. Usage lands on the RECEIPT rather than the outcome, because it is a
checkable fact about the run, which is what a receipt is for.

### `verbs` — two new, `SubagentSpawn` unchanged

```haskell
-- | Begin a coupled spawn carrying dynamic tools, and drive the turn to its
-- first stop. `tools` is a JSON array of {name, description, inputSchema};
-- `schema` is the terminal result's JSON Schema. Prefer `spawnAgentWithTools`.
agentBeginRaw :: SpawnSpec -> Value -> Value -> M (Either SpawnError AgentStep)

-- | Answer the parked tool call and drive on. `ok` false is a REFUSAL the
-- child reads and reacts to — not a transport error, and never a way to leave
-- the call unanswered. Prefer `spawnAgentWithTools`.
agentResumeRaw :: AgentId -> Text -> Bool -> Value -> M (Either SpawnError AgentStep)
```

Rust methods: `subagent_begin(spec, tools, schema)` and
`subagent_resume(agent, call_id, ok, body)`, both errors-tagged
(`Result<AgAgentStep, SpawnError>`, no `cx`).

A `tools` array element that does not parse is `SpawnDriveFailed StageAllocating`
— nothing has been allocated when the declarations are still being read.

---

## Frozen: `haskell/lib/Tidepool/Agent/Spawn.hs`

```haskell
-- | How many tool-call rounds the parent will serve before it stops
-- dispatching. POLICY, and the resident's: past the cap the loop answers a
-- refusal instead of dispatching, so the child finishes its turn normally
-- rather than being interrupted. Distinct from the runtime's MAX_TOOL_ROUNDS
-- backstop, which is a catastrophe bound, not a budget.
newtype ToolRounds = ToolRounds Int

-- | What a parent handler answered with.
data ToolAnswer = ToolAnswered Value | ToolRefused Text

spawnAgentWithTools ::
  forall tools r.
  (HasAgentApi tools M, FromJSON r, JsonSchema r) =>
  ToolRounds -> tools (AsServerT M) -> SpawnSpec ->
  M (Either SpawnError (SpawnOutcome, r))

-- unchanged signature; becomes spawnAgentWithTools at zero tools
spawnAgent :: forall r. (FromJSON r, JsonSchema r) => SpawnSpec -> M (Either SpawnError (SpawnOutcome, r))
```

Body: `compileTools` ONCE → declarations → `agentBeginRaw` → loop
{`dispatch` → `agentResumeRaw`} → `StepDone` → decode payload as `r`.

Rules the loop must honor:

1. **A `ToolCompileError` is a typed `SpawnError` returned BEFORE `agentBeginRaw`
   is called** — nothing is allocated, nothing is bound, no process is spawned.
   Spell it `SpawnDriveFailed StageAllocating (renderToolCompileError e)`.
2. **A call naming an undeclared tool is refused, never dropped.** `dispatch`'s
   own fallthrough `error`s, so the loop must check `dispatchNames` first and
   answer `ToolRefused` — an `error` inside a dispatch would abort the eval with
   the child's turn still parked.
3. **Past the cap, refuse — do not interrupt.** The refusal text names the cap so
   the child can act on it ("tool-call round cap reached (N)").
4. Decoding the terminal payload is unchanged from today's `spawnAgent`, and
   `SpawnResultMalformed` stays Haskell-produced only.

---

## Mock policy (root/human, 2026-08-11) — binding on every child

1. `MockBackend` stays a DUMB seam implementation. Zero Codex protocol
   semantics. By the fold it must be **no smarter than it is now**.
2. Protocol behavior is proven by the REAL adapter over a RECORDED transcript,
   not by an imitation. A test that can only pass by teaching the mock protocol
   behavior means either the seam is in the wrong place or you need a recording.
3. Hand-scripted fixtures remain correct for deterministic harness-contract
   tests ("given exactly this event, the loop does X") — arrange-step input.

---

## Receipt rule (this directory's, binding)

A gate that exists to catch ONE failure mode passes **by name**, with its own
pass line and the base commit it ran at. Never report only the aggregate.
Report completed-vs-selected, and name the instrument that produced any number.
Pass `--no-fail-fast`.

## Operational

Wrap bare `cargo` in `/home/inanna/dev/tidepool/scripts/ghc-slots.sh detach --`
(absolute path, never `exclusive`, at most ONE slot-taking leg at a time). Do
NOT wrap `scripts/battery.sh` — it takes its own slot.

**No live model calls.** Not in any test, not in any script you run. The one
live run in this lane is the TL's, on the human's granted budget.
