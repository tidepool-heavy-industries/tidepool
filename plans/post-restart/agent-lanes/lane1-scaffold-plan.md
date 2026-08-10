# Lane 1 scaffold plan — one-cycle coupled-spawn vertical (agent-core)

Written before spawning anyone, per the lane spec. This names the dev
decomposition, the frozen-but-provisional contracts, and the interpretation
calls a successor would otherwise have to re-derive. Design authority:
PRD 18 + its 2026-08-09 addendum, PRD 19, and
`inheritance-for-agent-core.md`. Everything here is LANE 1 ONLY — one worker,
one cycle, no cross-cycle, no reattach, no pokes, no mailbox.

## The vertical

```text
Haskell: spawnAgent spec task (schema for r)
  └─ Rust saga (ONE call, atomic):
       Allocating        resolve workspace (create managed worktree OR take an
                         existing UNBOUND one by id)
       WorktreeReady     handle in hand
       Bound             BindingTable.bind(worktree, agent-<id>)
       ThreadAccepted    backend.start_thread (ephemeral, dynamic_tools=[])
       Running           backend.run_cycle (cwd=worktree, task, outputSchema)
  └─ typed SpawnOutcome back to Haskell: WorkerRun{agent, worktree, thread}
     + CyclePayload + SpawnReceipt{worktree id, binding ref, thread id, EXACT
     resolved model}
  └─ Haskell decodes the structured payload into the caller's type r
     (Generic, named-field polarity) — decode failure is SpawnResultMalformed,
     never a success
```

## Interpretation calls (made here, flagged as such)

1. **"No orphaned worktree" under retain-first.** `tidepool-worktree` never
   deletes (locked). So saga rollback does NOT remove a created worktree; it
   settles the binding (`Released`) so nothing is left Active-bound to an
   agent that will never run. The rolled-back end state is: worktree retained,
   discoverable in the registry, UNBOUND, rebindable. "Orphaned" means
   "Active-bound to a dead agent", not "exists". Tests assert exactly that
   from DISK state (reopen the BindingTable after the failure).
2. **Success settles the binding `Terminal`.** A lane-1 agent is one cycle by
   construction; when the cycle completes the agent is terminal, so the
   binding settles `Terminal` (distinct from rollback's `Released` — finished
   vs stopped-waiting, per binding.rs's own doc).
3. **Effect GADT is named `Subagent`, helpers spell `spawnAgent*`.**
   `Tidepool.Agent` (module) is taken by the harness answerer; PRD 18's
   public-surface rename is root's call (inheritance §8). The helper names are
   the authored API; the GADT name is row plumbing. Provisional.
4. **The lane-1 call is synchronous run-to-completion.** PRD 18's async
   handle/`waitAgent` surface is lanes 2–5. One call in, typed outcome or
   typed error out. Provisional shape, allowed by the lane charter.
5. **`Subagent` requires `Worktree` in the same row.** Its types reference
   `WorktreeSpec`/`WorktreeHandle`/`WorktreeError` from `worktree_effect_def!`'s
   type_defs. A Subagent-without-Worktree row won't compile its generated
   module. Acceptable for lane 1; noted for the vocab/row story.
6. **AgentId is minted in-process (u64 counter).** A durable agent registry is
   lifecycle territory (lanes 2/4/5) — routed in the handoff, not built.
7. **No dynamic tools in the lane-1 authored surface.** The seam carries
   `Vec<DynamicToolDeclaration>` (transport supports it; agent-wave proved it
   live) but lane 1 passes `[]` — parent-tool dispatch through the realm is
   its own lane. The one-cycle result rides `outputSchema` only.

## Frozen (provisional) contracts — in the scaffold commit

Rust, `tidepool-agent`:
- `seam.rs` additions: `ModelPolicy::CheapPlumbing` (runtime-resolved; the
  receipt records the EXACT model — a receipt naming a tier is not checkable),
  `ThreadSpec`, `CycleSpec`, `CycleResultPayload{Structured|Unstructured|Absent}`,
  `CycleOutcome{turn, payload, activity, resolved_model}`.
- `backend/mod.rs`: `trait OneCycleBackend { start_thread; run_cycle }` —
  sync trait (handlers are sync; the codex impl owns its tokio runtime
  internally, LlmHandler precedent).
- `backend/mock.rs`: `MockBackend` — scripted payload, injectable failure at
  thread-start or cycle, call log. THE test backend; no live model anywhere in
  tests (standing rule, Inanna 2026-08-09).
- `spawn.rs`: `SpawnStage`, `SpawnError` (typed, stage-carrying),
  `SpawnWorkspace{New(WorktreeSpec)|Existing(WorktreeId)}`, `SpawnRequest`,
  `WorkerRun`, `SpawnReceipt`, `OneCycleRun`, `CoupledSpawner` (owns
  `WorktreeManager` + `BindingTable`; `spawn_one_cycle` stubbed for dev A).

Wire, `tidepool-bridge-effects`: `Ag*` mirrors (field order == type_defs
order, positionally — the wire contract).

Effect def, `tidepool-mcp/src/effect_defs.rs`: `subagent_effect_def!` — one
verb `SubagentSpawn(spec, schema) -> Either SpawnError SpawnOutcome`, errors
block mirroring `spawn.rs`, helpers `spawnAgentRaw` + spec builders.
Registered in `effect_decls.rs`; NOT in `base_effects!` (opt-in row, like
Worktree/RepoEvent).

Handler skeleton, `tidepool-handlers/src/handlers/agent.rs`: macro invocation
+ `SubagentHandler{spawner, backend: Box<dyn OneCycleBackend + Send>}` +
stubbed `subagent_spawn` — so the scaffold commit compiles both projections
end to end before anyone forks.

Haskell (dev D's file, shape frozen here): `Tidepool.Agent.ModelCodec` — ONE
Generic traversal yielding `modelSchema :: Proxy a -> Value` (JSON Schema,
NAMED fields, snake_case selectors) and `decodeModel :: Value -> Either Text a`
(reads the same named-field shape). This is the MODEL-boundary polarity — the
wire caveat: gate 1(b)'s positional `{tag, fields:[..]}` shape is WRONG here;
live tool arguments and outputSchema output are named-field objects. Sums
become tag-discriminated objects (`{"tag": "Completed", "summary": ..}` /
schema `oneOf` with a `const` tag). Scope: records + sums of records/nullary,
fields Text/Int/Bool/lists/nesting. Acceptance type: PRD 18's `WorkerResult`.

## Dev decomposition

Wave 1 (parallel, sonnet devs; heavy legs sequenced, builds niced):

| Dev | Task | Boundary |
|---|---|---|
| `saga` | implement `CoupledSpawner::spawn_one_cycle` + rollback + pure-Rust tests against REAL temp repos (worktree-crate convention: never mock git) with MockBackend failure injection | `tidepool-agent/src/spawn.rs`, `src/backend/mock.rs`, `tests/` |
| `codex-live` | `CodexOneCycleBackend` over `Session`/`drive_turn` (model/list resolution: prefer gpt-5.4-mini, else gpt-5.6-luna, NEVER terra; cwd at turn/start only; ephemeral) + `examples/live_one_cycle.rs` + live-leg doc. NO live runs by the dev — compile + fixture-replay tests only | `tidepool-agent/src/backend/codex/`, `examples/`, `plans/post-restart/agent-lanes/lane1-live-leg.md` |
| `handler` | fill `subagent_spawn`: wire↔domain conversions, saga call, error map; handler unit tests (mock backend, temp repos) | `tidepool-handlers/src/handlers/agent.rs` (+ effect-def mechanical fixes, flagged) |
| `model-codec` | `Tidepool.Agent.ModelCodec` per the frozen shape (+ optional `Tidepool.Agent.Spawn` typed wrapper, Form.hs precedent: `import Tidepool.Effects (...)`) | `haskell/lib/Tidepool/Agent/ModelCodec.hs`, `Spawn.hs` |

Wave 2 (after wave-1 folds):

| Dev | Task |
|---|---|
| `acceptance` | `tidepool-handlers/tests/subagent_one_cycle.rs` — standalone parked-path driver (repo_event_with_handler.rs pattern): (a) happy path, mock payload → Haskell `WorkerResult` via ModelCodec, receipt fields asserted; (b) injected thread-start failure → Haskell case-matches `Left (SpawnBackendFailed ...)`, then DISK assertions: no Active binding, worktree retained; (c) malformed payload → `SpawnResultMalformed`, not success. `require_*` fail-loud guard, no skip-as-pass |

TL (me): scaffold, folds, full verify battery, lanes-2-5 routing handoff,
submit_branch.

## Verification map

- Fast tier (unbrokered): `cargo nextest run -p tidepool-worktree -p tidepool-agent`
- Brokered, one at a time, via PARENT `/home/inanna/dev/tidepool/scripts/ghc-slots.sh detach`:
  targeted `-E` legs of `tidepool-handlers` (handler unit tests, acceptance
  binary). NO full shards — a tidepool-runtime shard is already running on
  this box.
- `cargo check --workspace --all-targets`, clippy on touched crates, fmt.
- Builds: `nice -n 15`, `CARGO_BUILD_JOBS=4`.
- Live leg: NOT run by any agent. Documented invocation + expected receipts in
  `lane1-live-leg.md`; a human triggers it.
