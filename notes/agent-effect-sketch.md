# Agent Effect — Interface Sketch

> Design session 2026-07-02/03, Inanna × Claude (Fable 5, i.e. the thing whose
> Workflow interface this replaces). Product of a live dogfooding session on
> the oneshot eval server followed by a decision-by-decision poll. Status:
> **brainstorm ripened into a settled sketch** — every major fork below was an
> explicit choice with rationale, not a default. Not yet a plan; promote to
> `plans/` when someone picks it up.

## Thesis

Claude Code's Workflow tool is a JS DSL that re-invents monadic composition:
`pipeline()` vs `parallel()` is Kleisli-chains vs traverse-with-barrier,
null-filtering is `Either` without types, the "patterns chapter"
(adversarial verify, judge panel, loop-until-dry) is prose teaching what a
typed combinator library would enforce. Tidepool already owns the two assets
that DSL lacks: **a serializable parked continuation** and **a typed effect
plane**. The Agent effect cashes those in: Haskell composes agents; each
agent has structured input (a per-agent payload lane) and structured output
(a schema-validated result).

The through-line of the settled decisions: **boring kernel, rich library**.
Full Claude Code agents (no novel substrate), no mid-flight dialogue, caps in
a config file, shell-out driver. The two ambitious bets deliberately kept are
the two that differentiate:

1. **The ADT is the schema, the Haddock is the prompt** — authoring JSON
   Schema by hand is the worst part of writing a Workflow; here it's a
   `data` declaration with `-- ^` comments.
2. **Park-once free-applicative fleets** — parallelism falls out of data
   dependencies, one continuation park covers an entire fan-out, and it
   generalizes past agents to any host-runnable effect (`run`, `httpGet`).

---

## The interface

### Kernel effect (4 constructors; everything else is library)

```haskell
data Agent r where
  Spawn    :: Value{-spec-} -> Value{-schema-} -> Agent AgentHandle
  Await    :: AgentHandle   -> Agent (Outcome Value)
  AwaitAny :: [AgentHandle] -> Agent (AgentHandle, Outcome Value)
  Poll     :: AgentHandle   -> Agent AgentState
```

Wire types are `Value`-level (the schema and result cross the hylo boundary
as JSON); typing lives in the Haskell layer via `AgentIO` (below). `AwaitAny`
is the load-bearing primitive — it makes pipeline semantics a user-space
event loop instead of a framework concept.

### Specs — builders, never bare records

```haskell
data Tier = Haiku | Sonnet | Opus | Inherit

haiku, sonnet, opus :: Text -> AgentSpec        -- tier + task
withContext :: AgentSpec -> Value -> AgentSpec  -- per-agent input payload lane
withLabel   :: AgentSpec -> Text  -> AgentSpec  -- progress display name
withPhase   :: AgentSpec -> Text  -> AgentSpec  -- progress grouping
inWorktree  :: AgentSpec -> AgentSpec           -- isolation opt-in (Shared default)
allowTools  :: AgentSpec -> [Text] -> AgentSpec -- --allowedTools passthrough
withEffort  :: AgentSpec -> Effort -> AgentSpec
```

An agent is *an eval with input + schema* — `withContext` is the same
payload-lane idea as the eval `input` param, per agent. The symmetry is
deliberate; document it as such.

### Typed I/O — `AgentIO`

```haskell
data Finding = Finding
  { file :: Text  -- ^ repo-relative path, e.g. "tidepool-heap/src/arena.rs"
  , line :: Int   -- ^ 1-based
  , sev  :: Sev   -- ^ triage severity; High = worth a ledger entry
  , why  :: Text  -- ^ one sentence, cite the code you actually read
  }
deriveAgentIO ''Finding
```

`deriveAgentIO` (TH, runs at extract time with full type info) generates:

- `schemaOf @Finding :: Value` — a real JSON Schema, **including per-field
  `description`s read from Haddock via `getDoc (DeclDoc name)`** (GHC ≥ 9.2;
  requires `-haddock` in the extract pipeline — one flag). Sum types like
  `Sev = Low | High` become enums.
- `decodeOf @Finding :: Value -> Either [Violation] Finding` — the validator/
  constructor, emitted as ordinary Core the JIT runs like any code.

The prompt-quality knob IS the Haddock. Escape hatch for doc-less or
third-party types: `described [("file", "…"), …]` schema override.

### Failure — data, never throw; loudness opt-in

```haskell
data Outcome a = Done a | Failed Failure
data Failure   = BadOutput [Violation] Value  -- surfaced only after host retries
               | Died Text                    -- process/API terminal error
               | Skipped                      -- caller/gate declined
               | OverBudget                   -- config ceiling hit

agent :: AgentIO a => AgentSpec -> M (Outcome a)   -- spawn + await
fleet :: AgentIO a => [AgentSpec] -> M [Outcome a]
```

- Schema-validation misses are retried **host-side** N times, feeding the
  violations back to the agent (same validate-don't-consume loop `resume`
  already implements). The orchestrator never sees transient noise.
- Nothing throws. A 50-agent fleet with one dead agent returns 49 `Done`s
  and one `Failed` — partial results are the point of a fleet.
- Opt-in loudness is free Haskell: `Done plan <- agent spec` (pattern-bind
  failure) when crash-on-failure is what you want; full `case` when it isn't.
  The type makes handling the path of least resistance.

### Parallelism — free applicative over host-runnable requests

**No freer-simple surgery.** `Eff`'s `Applicative` stays `ap`/sequential.
Parallelism is a separate free-applicative layer whose leaves are
host-runnable requests, interpreted as one batch:

```haskell
parA   :: M (Outcome a) -> Par a   -- lift an agent call
runPar :: Par a -> M a             -- spawn all leaves, park ONCE, wake with all

fleet specs      = runPar (traverse (parA . agent) specs)
concurrently a b = runPar ((,) <$> parA a <*> parA b)
```

The single Haskell continuation parks once while the host runs every leaf
concurrently — the existing parking machinery pointed at N children instead
of one ask. `Par` is plain data (JIT-friendly, no magic).

**Generalization worth building toward:** make `Run`/`HttpGet` `parA`-liftable
too, so `(fixes, tests) <- runPar ((,) <$> parA (fleet fixSpecs) <*> parA (run "cargo test"))`
runs agents *while the test suite runs*. This falls out of the same
interpreter if async-able effects share a host-request representation.

Handles (`spawn`/`awaitAny`) stay public underneath for reactive scheduling
the applicative can't express (work-stealing, spawn-on-landing, tournaments).

### Swarm.hs — the face (Workflow's patterns chapter, compiled)

```haskell
pipelineOver :: [i] -> (i -> M (Outcome a)) -> (a -> M [b]) -> M [b]
  -- awaitAny event loop inside: NO barrier — item A's stage-2 fires the
  -- moment its stage-1 lands, while item B is still in stage 1
judgePanel   :: AgentIO v => Int -> (Int -> AgentSpec) -> M [Outcome v]
quorum       :: Int -> [Outcome Verdict] -> Bool
adversarial  :: AgentIO v => Int -> Text -> M Bool   -- N refuters, majority
untilDry     :: Int -> M [a] -> M [a]                -- K empty rounds = done
harvest      :: WorktreeRef -> M Patch               -- dirty worktree → Patch machinery
```

These are 5-liners over the kernel + existing `Schemes` (`loopM`, `untilM`,
`foldEarlyM`, `retry` already exist and compose directly). Saved workflows =
`.tidepool/lib` verbs: typed, composable inside other workflows, sedimentation
ladder already built — strictly better than Workflow's stringly script files.

---

## Host side (as settled)

### Consent & caps — server config only

```toml
# .tidepool/config.toml
[agents]
max_concurrent      = 8
max_tier            = "sonnet"
daily_token_ceiling = 2_000_000
```

No per-eval declaration, no interactive gate. Spawn past a ceiling is
`Failed OverBudget` — an outcome like any other, handleable in-plane.
(Rationale: matches how `run`/`httpGet` are already trusted; the config file
is the same trust boundary the sandbox root uses.)

### Progress — structured, on the park

While a fleet runs the eval is parked; the suspension carries a live agent
table the client can poll/render without disturbing the park:

```json
{"paused": true,
 "phase": "Verify",
 "agents": [
   {"label": "review:bugs",   "state": "done",    "tok": 41000},
   {"label": "review:perf",   "state": "running", "tok": 12000},
   {"label": "verify:heap#3", "state": "queued"}],
 "spent": "213k / 500k"}
```

This is Workflow's progress tree, as data on the continuation. `withLabel`/
`withPhase` feed it.

### Driver — headless CLI behind a trait

```
trait AgentDriver { fn spawn(&self, spec) -> HandleId; fn poll(&self, id) -> AgentState; }
struct CliDriver;   // v1: claude -p --output-format json --allowedTools … (cwd = worktree)
```

**Honesty note:** Workflow does NOT shell out — its subagents are in-harness
spawns (shared process family/MCP plumbing, warm start). From outside the
harness, `claude -p` is the equivalent. Deltas: cold start per agent, no
shared MCP/session state with the caller, agents bill/configure off the
user's install. **Cold start assessed as noise** (Inanna: LLM inference
dominates process startup by orders of magnitude) — no economics pressure
toward fewer-bigger agents. The `AgentDriver` trait keeps the Agent-SDK
daemon option open without building it (a second driver must exist before
the seam is proven — do-it-right rule applies).

### Isolation & merge — Workflow parity

`Shared` working tree by default; `inWorktree` is per-agent opt-in for
parallel mutators (Workflow's exact posture, incl. auto-clean if untouched).
A dirty worktree comes back in the `Outcome` as `{worktree, branch, diffstat}`;
integration is the orchestrator's job via `harvest → planDiff → conflicts-as-
data → applyDiff` (the Patch machinery). Patch-as-value (agent returns a
unified diff in its typed output, worktree discarded) remains available as a
*prompting pattern*, not an enforced mechanism.

---

## Decision log (each was an explicit fork)

| # | Decision | Chosen | Rationale / rejected alternatives |
|---|----------|--------|-----------------------------------|
| 1 | Result typing | **ADT-derived schema (TH + Haddock docs)** | Per-field descriptions for the LLM were the hard requirement — `getDoc` satisfies it. Rejected: Schema+Value only (JSON dialect fossilizes as the idiom); Generic-based (dictionary-heavy Rep towers are historically JIT-hostile — would need a GO/NO-GO spike; TH at extract time sidesteps entirely). |
| 2 | Agent substrate | **Full Claude Code subagent, no tidepool access** | Simplest, highest utility, closest to Workflow power. Rejected (for now): eval-native agents (LLM loop whose only tools are eval/resume, effect-row-as-capability-grant) — the novel option, revisit once the boring one works. |
| 3 | Sub-agent asks | **None — fire-and-forget** | Underspecified tasks return `Failed (Underspecified …)`; orchestrator re-spawns with a better spec. Rejected: ask-bubbling lattice (tidepool's most distinctive capability, but zombie-agents-parked-on-questions makes fleets intractable; keeps recovery simple too). |
| 4 | Failure contract | **Enum return everywhere + host-side schema retry** | Workflow semantics (retry validation at the tool layer, errors as nulls) but typed: users default to *handling* because the type demands it; a swarm never hard-crashes because one LLM died. Loudness opt-in via `Done x <-` pattern bind. |
| 5 | Parallelism | **Free-applicative `Par` layer + public handles** | Viability confirmed: no freer-simple surgery, `Par` is data, one park per batch. Rejected: combinators-only (every shape needs a named verb); full ApplicativeDo (refactors silently change the schedule). |
| 6 | Edit isolation | **Shared tree default, worktree opt-in** | Workflow parity, confirmed after checking what Workflow actually does. Patch-as-value kept as pattern. |
| 7 | Consent/caps | **Server config only** | Same trust boundary as the rest of the effect stack. Rejected: per-eval budget declaration (ceremony), first-spawn interactive gate (policy fog on loops). |
| 8 | Progress | **Structured agent table on the parked suspension** | Poll-without-disturbing beats journal-tailing for the live view. |
| 9 | Recovery | **Deferred — "need to think about how to do it right"** | See below. |
| 10 | Driver | **`claude -p` behind `AgentDriver` trait** | Cold-start delta assessed as noise vs inference time. |

## Deferred: recovery (design note to bank)

A 40-agent fleet dying mid-flight (server restart, eval killed) currently
loses everything. When this gets taken up, the shape that fits:

- The progress-on-park machinery already forces the host to track per-agent
  `(spec, state, result)`. Persist that table keyed by **spec-hash** and it
  *is* the journal.
- Recovery UX = "run the same eval again": completed spawns hit the journal
  cache instantly, only missing agents launch live. No new user surface.
- Content-keyed beats Workflow's longest-prefix positional replay (survives
  reordering of the spawn sequence).
- Decision #3 (no asks) is what keeps this tractable: a completed agent is a
  pure `spec → result` mapping, no dialogue to replay.
- Open sub-questions: cache invalidation (same spec, changed working tree —
  hash the tree state in? opt-out flag?); TTL/GC for the journal; whether
  `Skipped`/`Failed` outcomes cache or always re-run.

## Open questions

1. **`Par` over non-agent effects** — how do `Run`/`HttpGet` join the
   applicative batch? Needs a common host-request representation; decide
   whether that's v1 or fast-follow. (The demo — agents working while
   `cargo test` runs — is strong enough to want early.)
2. **Schema retry budget** — N for `BadOutput` retries: fixed? per-spec?
   config? Workflow retries at the tool layer without a visible knob.
3. **Worktree lifecycle** — who cleans dirty worktrees whose outcomes were
   consumed but never harvested? (Ledger precedent: `.exo/worktrees` stale
   checkout noise found during the dogfooding session.)
4. **`Inherit` tier semantics** — inherit from what, exactly? The oneshot
   server has no ambient model; probably "config default" not "caller model."
5. **Haddock extraction breadth** — `getDoc` needs `-haddock` on the *eval
   module* compile; verify the extract pipeline can enable it globally
   without cost (or gate on the deriving splice being present).
6. **Repl-plane interplay** — a repl session holding fleet results in
   bindings across turns is the "session as orchestrator memory" story
   (Workflow's resume-journal exists because the JS orchestrator loses
   context; a session doesn't). Nothing to build, but document the idiom
   once the effect exists.
7. **Eval-native agents (rejected substrate) as v2** — the effect-row-as-
   capability-grant idea (agents whose whole world is a granted effect
   subset, thinking in the verb library) is the genuinely novel version.
   Parked, not killed: the `AgentDriver` trait is where it would slot in.

## Workflow ↔ Agent effect map (reference)

| Workflow (JS) | Agent effect |
|---|---|
| `agent(prompt, {schema})` + tool-layer validation retry | `agent :: AgentIO a => AgentSpec -> M (Outcome a)` + host retry (same resume-validation machinery) |
| errors → `null`, `.filter(Boolean)` | `Outcome a`, `[a \| Done a <- rs]` |
| `{label, phase, model, effort, isolation}` opts bag | `AgentSpec` builders |
| `pipeline()` (no barrier — the docs' main teaching burden) | `pipelineOver` / `awaitAny` event loop — the no-barrier property is code shape, not doctrine |
| `parallel()` (barrier) | `runPar` / `fleet` — barrier visible in the code |
| `budget.remaining()`, loop-until-budget | config ceiling + `OverBudget` outcome + `Schemes.untilM` |
| patterns chapter (prose) | `Swarm.hs` (typed 5-liners) |
| saved workflows (`.claude/workflows/` script files) | `.tidepool/lib` verbs — typed, composable, already have a sedimentation ladder |
| resume = positional journal replay (bans `Date.now`/`Math.random` in scripts) | deferred; content-keyed spec-hash cache when built (no determinism ban needed — parking is real suspension, not replay) |
| progress tree UI | agent table on the parked suspension |
| in-harness subagent spawn | `claude -p` via `AgentDriver` (cold start = noise) |
