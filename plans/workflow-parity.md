# Tidepool vs Dynamic Workflows: Parity Analysis & Roadmap

2026-06-11. Written the day tier-1 went live; empirical claims measured, not
guessed. The comparison target is the Claude Code Workflow feature: a
deterministic JS orchestration script spawning full Claude subagents —
`agent(prompt, {schema, model, isolation})`, `parallel()`/`pipeline()` with a
~10-16 concurrency cap, phase/log progress, shared token budget, journaled
resume, worktree isolation.

## The one-sentence positioning

Workflows scale **breadth** (tens of full agents, each expensive, each
fire-and-forget); tidepool scales **depth** (one cheap typed computation with
hundreds of micro-judgments and *surgical, mid-flight* escalation). The
endgame is not cloning workflows — it is being the thing workflows can't be:
a **conversational, typed, 100x-cheaper orchestration substrate** where the
big LLM is a coroutine peer, not a fire-and-forget spawner.

## Scorecard (today)

| Workflow capability | Tidepool today | Gap |
|---|---|---|
| Deterministic orchestration script | Haskell computation (loopM, hylo, typed combinators) — STRONGER language | none |
| `agent()` full subagent | `??`/`llmJson` one-shot judgments; `seekAuto` = micro-agent over a closed move vocabulary | no general tool-loop agents |
| `schema` structured output | Q schemas end-to-end, OpenAI strict-enforced, validated resume | AHEAD (enforced both tiers) |
| `parallel()`/`pipeline()` | Sequential within a computation; 4 concurrent evals at caller level (MEASURED: windows overlap) | no fan-out inside a computation |
| Per-agent `model` tiering | One server-global model + ask-tier (the caller) | no per-call model choice |
| Token budget | LLM_MAX_CALLS=200 count cap per eval (per-eval since today's fix — was accidentally server-lifetime) | no token/cost telemetry |
| Journaled resume | KV scratchpad (manual checkpoints); CBOR compile cache | no effect-journal replay |
| Progress (phase/log) | say/[move] log — delivered only at completion/suspension | no live streaming |
| Failure isolation (agent→null) | One effect error/JIT trap kills the whole computation | no `try` at effect boundary |
| Worktree isolation | none (direct Fs writes; Exec could shell to git worktree) | unaddressed |
| — | **Mid-flight suspension to the caller** (ask/askQ, typed, validated, abortable, draft-carrying) | workflows have NOTHING like this |
| — | ~$0.0002/judgment, 1-2s latency vs agent-spawn $0.01-1, 10-60s | 100x cost/latency advantage |

## Measured today

- **Caller-level parallelism works**: 3 evals fired in one message; B[379-424]
  and C[387-432] fully overlapped (A finished before B started its timed
  region — compile serialization suspected, needs a warm-cache rerun).
  `MAX_CONCURRENT_EVALS=4` is the cap. The caller IS a workable `parallel()`.
- **Escalation tax on batch ops**: an 8-item sift took 45s wall vs ~8s
  judgment time — 3 per-item escalations at default bar 0.6, each a full
  caller round-trip. All three drafts were CORRECT (rubric pessimism, again).
  Per-item escalation is the wrong shape for batches.
- **Mini-as-driver boundary** (5 seekAuto runs): succeeds when the goal
  contains greppable anchors; fails honestly (guards prevent hallucination)
  when the answer requires synthesizing a search token. Conclusion-readiness
  and self-confidence calibration are its weak skills; move quality is good.
- **Bugs flushed by orchestration load**: server-lifetime LLM budget
  (fixed: per-clone counter); sliced-Text bridge trap at effect boundary
  (#313 family, worked around in unquote, repro documented).

## Roadmap, by leverage

### 1. `LlmBatch` — parallel judgment fan-out (the big rock)
New Llm op: `LlmBatch :: [(Text, Value)] -> Llm [Value]` (prompts+schemas →
results). Handler does `join_all` on the tokio handle — the eval thread
blocks once for the whole batch. Haskell: `siftPar`, `triagePar`, `surveyPar`
built on it. 50 judgments: ~50s → ~2-3s. This single op closes most of the
practical `parallel()` gap because tidepool's unit of work is the judgment,
not the agent. Trivial Rust (the handler already holds the rt handle).

### 2. Unsure-aggregation — batch escalation in ONE suspension
Batch ops run `?!` at bar 0 collecting `Judged`; Unsure items accumulate and
escalate ONCE: a single askQ carrying ALL low-confidence items + drafts as an
array schema. N items → ≤1 suspension (vs N). Pure lib code, buildable today.
Kills the measured 4-5x escalation tax. `siftJ :: Q Bool -> (a -> Text) ->
[a] -> M ([a],[a])` with a final bulk-review round.

### 3. `try` — failure isolation (old plan item B, now load-bearing)
`TryOp` wrapper at dispatch: handler errors → `Left msg` values instead of
eval death. A 30-min orchestration must survive a 404. Rust: catch at the
AskDispatcher/inner dispatch boundary behind a marker effect; Haskell:
`try :: M a -> M (Either Text a)`. Without this, long-running parity is
fiction — one bad probe kills an hour of work.

### 4. Model tiering per call
`LlmStructuredAs :: Text -> Text -> Value -> Llm Value` (model override;
genai routes by name already). Q gains `via :: Text -> Q a -> Q a`. Then
cascades become: deterministic → mini → gpt-4o/claude → caller. Cheap.

### 5. ~~Spawn~~ → CUT (user steer 2026-06-11): parallel-eval only
No in-computation child agents — background sessions introduce eviction/
observability/abort question-clusters we don't want. Parallelism is the
CALLER's: fire N evals in one message (measured working), KV as blackboard.
SHIPPED instead (97c6108): **timeout-as-yield** — the pause gate. An eval
only computes during an MCP call; at window expiry it parks at its next
effect boundary and returns {"paused", continuation_id, output}; resume runs
another window; abort kills cleanly. Long computations stop having a 120s
cliff, which also makes caller-chunked parallel batches viable (each chunk
pauses instead of dying). Pure-compute runaways: grace-then-detach (old
behavior, reserved for exactly that case).

### 6. Telemetry & live progress
- genai returns usage; accumulate per-eval tokens; expose `llmSpent :: M Int`
  + budget arg on eval. Enables `while budget-remaining` loops (workflow
  budget parity).
- MCP progress notifications for streaming the say/[move] log mid-eval
  (rmcp supports notifications): turns seekAuto runs into live progress
  trees. Without it, long autonomous runs are black boxes until they return.

### 7. Durability (deliberately last)
Effect-journal replay (key effects by (source-hash, index); replay on rerun)
is the real resume analog, but ask-replay semantics and nondeterministic
effects make it a design project, and KV checkpointing covers the practical
cases today. Revisit when a real >10-min orchestration exists and hurts.

## Exceed-parity bets (lean into what workflows can't do)

- **Conversational orchestration**: aperture/ask mid-computation — already
  shipping, already beyond. Combine with Unsure-aggregation and the caller
  becomes a low-frequency, high-leverage supervisor of cheap autonomous work.
- **Typed agent protocols**: seekDriveR's vocabulary+guards as the general
  pattern — agents whose entire move surface is parsed, validated, and
  mechanically guarded (evidence-gated DONE, dup ledger). Workflow agents
  are prompt-disciplined; tidepool agents are contract-disciplined.
- **Cost structure**: a 500-judgment audit ≈ $0.10. The same audit as 500
  haiku agents is dollars and an hour. Whole categories of "too expensive to
  check" become routine (every-commit gotcha triage, doc-drift sweeps).
- **The caller-as-scheduler hybrid**: I can fire 4 concurrent evals and join
  them — the conversation itself is the workflow runtime, with KV as the
  blackboard. No new infra needed; needs MAX_CONCURRENT_EVALS bump + the
  compile-serialization question answered.

## Open questions

- Do all 4 permits truly overlap post-compile-cache? (rerun the timing test
  with warm sources)
- Suspension + eviction interaction under parallel load: oldest-suspended
  gets evicted by NEW eval admission — parallel orchestrations with asks can
  cannibalize each other. May need permit reservation or eviction exemption
  for actively-retried continuations.
- LLM_MAX_CALLS=200 right size for batch era? (LlmBatch counts as N.)
