# The Tidepool Harness — Capabilities Briefing

An orientation document for LLM collaborators: what this system is, what it
can do today, what is chartered next, and how to work on it well. It is
self-contained; pointers to deeper repo docs are collected at the end.

Everything below is marked either **live** (landed, running, tested) or
**chartered** (locked design in PRD 20, not yet built). Do not conflate them.

## What this system is

Tidepool compiles Haskell effect programs (freer-simple effect stacks) through
a GHC Core extractor into native code on a Cranelift JIT, drivable from Rust.
Effects are typed; an effect the runtime does not handle **suspends** — the
continuation is stowed as data, resumable by identity, in any order. That
suspension mechanism is the foundation everything here builds on.

The **harness** is the resident layer on top: you author a program —
`State`, `render`, `loop` — in plain Haskell, and the runtime
(`tidepool-selfharness`) drives it forever: render the state, open model
windows for cognition, run the authored loop, checkpoint, repeat. Xmonad
vibes: the config is code, the runtime is invisible.

The design philosophy, in two rules that explain most decisions:

1. **The API is the prompt.** Models are natively fluent in Haskell from
   training data. The surface mirrors canonical Haskell/GHCi; every deviation
   is a fluency tax.
2. **The interface evolves as an optimization loop.** Real frictions from
   clean-context model usage drive changes — never speculative design.

## The authored harness (live)

Two files per harness (see `harness-dogfooding/`):

- `HarnessTypes.hs` — `State`, the answer ADTs, and pure
  `render :: State -> Text`. No runtime handles, no `loop` reference, so
  answer windows and checkpoint codecs import vocabulary without the effect
  row.
- `Harness.hs` — `loop :: State -> Harness State`, the one effectful piece
  (`Harness` is an alias for `M`, the effect monad).

The driver checkpoints `State` (via `ToJSON`/`FromJSON`) every loop
iteration, restores on boot (carrying state forward across harness edits when
the shape still decodes), compacts the model transcript when context grows,
and enforces round caps with a grace-window glide instead of a hard kill.

## Cognition windows (live)

`runLLMTurn @T prompt` opens a clean-context model window that sees the
rendered state plus the prompt, writes Haskell, and must deliver a `T`:

- **The answer is type-pinned by the effect row.** The window compiles
  against `Finalize T` in its row; a wrong-typed answer is a GHC error fed
  back for a corrective retry, not a runtime surprise.
- **Windows in one loop share context** — the companion's orient → decide →
  act chain is three windows seeing one conversation.
- **Answers can be functions.** `runLLMTurn @(State -> State)` works end to
  end: the finalized closure is delivered by handle into the loop's parked
  continuation, same heap, no serialization. Records of functions work too.
  This is the deepest capability: models author typed *behavior*, not just
  data.

What a window can call (its row is `[AskUser, Fork, ReadState, Finalize T]` —
deliberately minimal; no shell, no filesystem, no network):

- `askUser @T` — present a typed form to the human operator, derived from
  `T`'s `Generic` shape; the submission comes back as a typed value.
  Re-prompts on decode failure. `note "…"` posts non-blocking narration to
  the operator feed.
- `getStateJson` — read the loop's current state as JSON.
- `fork`/`forkAll` — bounded parallel sub-answerer windows (depth one).
- `finalize` — the typed yield. Spelling matters; see Sharp edges.
- Top-level pure declarations persist by name across loops (the "decl
  plane"), so a window can `define` helpers its successors reuse.

A model reply may contain **multiple fenced `haskell` blocks**; they run
sequentially, stopping at the first failure, reported as "first N blocks
succeeded, block X failed with Y." One reply = one model round.

## The authored loop's powers (live)

The outer row today: `RunLLMTurn`, `AskUser`, `Worktree`, `Subagent`
(Console/RepoEvent/Exec are chartered — S1-L1 below). Everything suspends to
the driver and is serviced there; nothing dispatches behind the model's back.

- **Typed subagents** (`spawnAgent @r spec`): spawn a headless coding agent
  (Codex backend today) for one complete cycle in a managed git worktree. The
  result schema is derived from `r`'s own `Generic`/`JsonSchema` metadata —
  the type the caller pattern-matches on IS the contract the agent is held
  to; malformed payloads come back as typed `Left (SpawnResultMalformed …)`,
  never defaulted. `spawnSpecIn (WorktreeId …)` rebinds a retained worktree
  from an earlier cycle. `spawnAgentWithTools` gives the child typed tools
  whose handlers run in the *parent's* Haskell — the parent services every
  child tool call and can refuse, rewrite, or escalate. One agent at a time
  today (lifted in S1-L2).
- **Managed worktrees** (PRD 19): `createWorktree`, `fromCurrentRepository`,
  `fromWorktree`, `allowDirtySnapshot` (hidden snapshot of a dirty source —
  never touches the user's branch). Worktrees are retained indefinitely
  (losing work is worse than accumulating it), tracked in a durable
  registry. **The runtime has no git workflow verbs** — no merge, no rebase;
  agents do git work with native tools, and the runtime *observes*.
- **Typed repository events** (PRD 19): `withHandler (headChanged tree)
  handler body` — lexically scoped subscriptions; handlers run in the
  surrounding effect row; no replay of old events; queue overflow poisons
  loudly. Repository observations are authoritative; agent prose is a claim.
- **Operator forms from the loop** — `askUser` works at the outer level too.

## Live dogfoods — existence proofs

- **companion/** — an open-ended persistent companion. Proves: multi-window
  OODA loops; `Turn { directives, edit }` answers carrying a closure beside
  data; memory as a **git store curated by a spawned Codex agent** (the
  harness batches `Remember`/`Modify`/`Forget` directives per loop, the
  curator files them under the store's own ruleset and returns a typed
  receipt with the fresh digest); retained-worktree rebinding; carry-forward
  state across harness edits. Running live, daily.
- **dev-tree/** — a recursive development tree: `DevPlan` unfolds depth-first
  into implementation agents (parent first — children seed from finalized
  parent HEAD), then fresh integration agents merge bottom-up. Typechecks
  against its full row today; *runs* once S1-L1 servicing lands. This
  harness is the executable design target of PRD 20.

## Operator surface (live)

`tidepool-web` serves a local GUI: typed form rendering for `askUser`, an
accumulating note/narration feed, scrollable turn history of all compiled
Haskell, continue-gates between loops. Observability is two durable JSONL
streams (loop-level transcript; fine-grained per-turn log with every executed
block, hole, and answer). Turn compiles are content-addressed and memoized.

## The chartered future — PRD 20, "Exomonad v3" (locked design, not built)

The swarm successor to exomonad: coordination as a compiled resident program,
cognition only at typed seams. The locked core, compressed:

- **The swarm is a monadic hylomorphism; agents are its algebra and
  coalgebra.** `hyloM alg coalg` over `PlanF a = PlanF { task, kids :: [a] }`.
  `decompose` = planning window + parent-first scaffold (lazy: each layer is
  planned with the parent's real outcomes in hand). `integrate` = leaf
  implementation or merge agent + checks. The plan never materializes; git
  plus an append-only **run journal** persist everything; resume folds the
  journal ("from last good"), adopts orphaned commits after verification.
- **Policies are middleware** over the two seams (`receipted`, `budgeted`,
  `gated`, `capped`) — plain function wrappers, testable against pure
  algebras. **Policy slots are effectful** (`a -> M b`): a gate tiers
  deterministic heuristics → a specifically-prompted model turn → the
  operator.
- **Interior nodes are residents with mailboxes.** Each node is a green
  thread (over the parked-continuation substrate) owning its worktree
  exclusively, selecting over child folds, its inbox, worker completions,
  and repo events. Handles are the addressing — possession is permission,
  tree-edges-only by lexical scope. Messages (`RebaseOnto`, `AmendSpec`,
  `Cancel` / `Escalate`, `Progress`) are reconciliation hints re-derivable
  from git — crash-safe by design. The select loop is stdlib plumbing;
  harnesses supply policies.
- **Eager rebase cascade, tiered:** a fold landing on a node sends
  `RebaseOnto` down the tree; each owner tries the rebase mechanically
  (Exec-run git, abort on any conflict — zero tokens), spawns a resolution
  agent on conflict, escalates upward past that.
- **The trust ladder:** repository observations → orchestrator-run checks →
  adversarial review (fresh reviewer agents, findings as data, bounded
  rounds), with a typed **fold receipt** on every merge. A higher rung never
  overrides a failing lower one. Autonomy is policy over receipts: all folds
  operator-gated in v1; `SwarmCanCook` repos auto-fold green receipts.
- **Order-insensitivity is the concurrency contract:** completion order
  never reaches a policy input, so scheduling nondeterminism cannot change a
  decision. Swarm logic tests pass pure algebras — no agent processes.
- **Stage 1 lanes:** L1 row servicing (Console/Worktree/RepoEvent/Exec) →
  L2 concurrent agent cycles (`spawnAsync`, `agentDone` as an event) → L3
  the hylo swarm + dev-tree v2 → L4 green threads + node residency → L5
  resume hardening + rebase cascade → L6 operator surface. Stage 2: the
  resident factory (backlog, per-repo memory, autonomy policy). Stage 3:
  self-hosting.

Full text: `plans/self-iterating-harness/20-exomonad-v3-prd.md`.

## How to collaborate well

- **Locked decisions are final.** The Key Decisions Reference in the root
  `CLAUDE.md` and every PRD's "Locked decisions" section are settled —
  escalate to Inanna rather than re-derive or deviate. PRD 19's "no git
  workflow verbs in the runtime" and "retain worktrees, never delete on your
  own initiative" are the two most commonly tripped.
- **Design language:** conservative production Haskell — sum types with
  exhaustive case, `Either` for failures, newtype keys, `Map` state, derived
  `Functor`/`Foldable`/`Traversable`, records of functions for policies,
  pure decision cores in effectful shells. Branching logic and long
  functions are welcome; a niche feature earns its place only when nothing
  simpler expresses the thing (the hylo qualifies; phantom-type state does
  not — a wrapper plus a test says it plainly).
- **Receipts, not claims.** Observations outrank summaries everywhere:
  repository events over agent prose, checks the orchestrator ran over
  checks an agent reports, typed receipts over "done."
- **Prompt positively.** Tell models what you want, not what to avoid —
  "don't do X" instructions push models toward adjacent failure modes.
- **Plain language in anything Inanna reads.** No invented labels or
  acronyms; her vocabulary (receipts, journal, snapshots) over coined terms.
- **Frictions are the roadmap.** A live pain from an actual run outranks any
  speculative improvement.

## Sharp edges (each of these has burned a session)

- **`finalize` with a non-renderable payload** (one carrying a function)
  must be a bare expression with the annotation:
  `(finalize @Turn (Turn { … }) :: M ())` — never a bind
  (`_ <- finalize …`), never bare.
- **Sum types crossing JSON need record syntax on payload constructors**
  (`Blocked { blockedReason :: Text }`, not `Blocked Text`) — generic
  encoding has no key for a positional field; it fails when the derive is
  demanded, not at declaration.
- **Agent result types must be single-constructor records.** A sum renders
  `oneOf` at the schema root and the backend refuses the turn whole. Model
  an alternative as a field, never a constructor.
- **Use record-dot syntax** (`p.stdout`, `h.path`) — bare selectors are
  ambiguous across record types sharing field names.
- **`run cmd` returns `Right proc` even on nonzero exit** — spawn failure is
  the `Left`; inspect `proc.exitCode` (or `ok proc`).
- **The plan tree is data, the recursion is Haskell** — resist inventing
  workflow DSLs or graph engines; `traverse` and ordinary recursion are the
  orchestration language.

## Where the deep docs live (for collaborators with repo access)

- Root `CLAUDE.md` — project map, build/test tiers, locked decisions.
- `tidepool-harness/CLAUDE.md` — the harness runtime: sessions, realms,
  finalize contracts, closure delivery, machine lifecycle, logs.
- `tidepool-agent/CLAUDE.md` — the agent backend seam, containment, test
  tiers. `tidepool-worktree/CLAUDE.md` — worktree rules.
- `plans/self-iterating-harness/18-…`, `19-…`, `20-exomonad-v3-prd.md` — the
  PRD line: typed subagents, worktrees/events, the swarm.
- `harness-dogfooding/README.md` + `companion/`, `dev-tree/` — the living
  examples; read them before authoring a harness.
