# agent-wave lanes — PRD 18 (typed headless subagents)

TL spec: [`../agent-wave.md`](../agent-wave.md). Design authority:
[`../../self-iterating-harness/18-typed-subagent-spawning-prd.md`](../../self-iterating-harness/18-typed-subagent-spawning-prd.md).
This directory is the wave's plan/receipt namespace. Worktree-wave owns
`../worktree-lanes/`; do not write there.

## Receipt rule (swarm-wide, effective 2026-08-08 — binds every spec in this directory)

**A gate that exists to catch ONE specific failure mode passes BY NAME, with
its own pass line.** Never report only the aggregate that contains it.

A rename, an `#[ignore]`, a `cfg`, or an env-gated early return each leave a
green aggregate with the guard never executed — so "N/N passed" is compatible
with the one test you actually cared about not having run. A label never
establishes what its name implies.

Two companions:

- **Cross-lane guards also name the base commit they ran on.** Base proves the
  tree, name proves execution; both, or neither is established.
- **A cross-lane guard must sit INSIDE the guarded lane's gate set.** A guard
  that only runs in the guarding lane's suite does not protect the lane it
  names.

Applied to this wave's own gates, the tests that need named pass lines are the
ones asserting a *specific* failure is caught: the codec's loud-rejection pair,
the isolation checker's new-database-sidecar case, `compileTools`' single-
traversal invariant, and every diagnostics compile-fail fixture. For compile-
fail fixtures, "it failed to compile" is the weakest possible evidence —
show each failed for *its own reason* (the asserted message text), since a
fixture passing on an unrelated error is exactly what this rule exists to
catch.

## Wave 1 lanes

| Lane | Owns | Deliverable |
|---|---|---|
| `dev-adapter-bringup` | `tidepool-agent/` (Rust) | Config isolation proven, app-server handshake, protocol fixtures pinned |
| `dev-mode-encoding` | `haskell/lib/Tidepool/Agent/Contract.hs`, `tidepool-runtime/tests/agent_mode_encoding.rs` | Gate 1(a) verdict + the eDSL contract algebra |
| `dev-structural-codec` | `haskell/lib/Tidepool/Agent/CodecSpike.hs`, `tidepool-runtime/tests/agent_structural_codec.rs` | Gate 1(b) verdict: list + recursive ADT round-trip on the real JIT |

Wave 2 (after wave 1 folds) carries the adapter's live vertical:
task → `item/tool/call` → typed reply → resume → structured completion.

## Scaffold already committed

`tidepool-agent/` is the containment crate. Its `src/seam.rs` is the
Tidepool-owned vocabulary; `src/backend/codex/` is the ONLY place
`codex-codes`, app-server JSON-RPC, or the word "Codex" may appear. That
boundary is PRD 18's non-goal "making `codex-codes` types part of Tidepool's
public Rust or Haskell API", made structural.

## Pinned backend versions

- Codex CLI: **0.146.0** (`codex --version` → `codex-cli 0.146.0`), from the
  operator's nix profile.
- `codex-codes`: **0.146.4** (latest on crates.io; the crate tracks CLI
  versions, and 0.146.x is the matching family — the patch-level skew against
  the CLI is recorded deliberately, not assumed harmless, and any protocol
  mismatch found during bring-up is attributed here first).

Pinning is CLI-and-crate together. Dynamic tools are an experimental
app-server surface; a version bump re-runs the fixtures rather than refreshing
a lockfile.

## Protocol reconnaissance already done (TL, offline, zero token spend)

`codex app-server generate-json-schema --out <dir>` emits the complete
version-matched protocol schema from the pinned CLI. Verified: it does not
touch `~/.codex` (top-level file size/mtime snapshot identical before and
after). This is the cheap way to answer protocol-shape questions — read the
schema before spending a ChatGPT turn on the same question.

Established from the 0.146.0 schema:

- `item/tool/call` **is** a `ServerRequest` — a server→host request that awaits
  a host response. That is the park/reply primitive the whole design rests on,
  present and not merely documented.
- `DynamicToolCallParams` carries `{threadId, turnId, callId, tool, arguments,
  namespace?}`. `callId` is the correlation token; `threadId`+`turnId` are what
  make cross-agent misrouting detectable.
- `DynamicToolCallResponse` is `{success: bool, contentItems: [...]}` where a
  content item is `inputText`/`inputImage`/`inputAudio`. **A tool *error* is
  `success: false` with content, not a JSON-RPC error** — that is the shape a
  failing Haskell handler must produce so it never strands a pending call.
- `TurnStartParams` requires `{threadId, input}` and accepts `cwd`, `model`,
  `effort`, `outputSchema`, `sandboxPolicy`. `outputSchema` at turn start is
  therefore available for structured completion.
- `ThreadStartParams` has no required fields and accepts `cwd`, `ephemeral`,
  `model`, `sandbox`, `developerInstructions`, `baseInstructions`, `config`.
  Supplying `cwd` only at turn start is expressible — which is the shape PRD 18
  wants for avoiding the project-trust write.
- **RESOLVED by bring-up — and the generated schema was misleading here.**
  `dynamicTools` is a top-level field of `ThreadStartParams`, sibling to
  `cwd`/`config`/`model`, NOT nested in `config`. It is thread-scoped (frozen
  at creation, as PRD 18 assumed), gated by
  `#[experimental("thread/start.dynamicTools")]`, and unlocked by
  `InitializeParams.capabilities.experimentalApi = true`.

  The reason it looked absent: **schema generation silently drops
  `#[experimental(...)]`-gated fields**, so `DynamicToolSpec` and
  `DynamicToolNamespaceTool` appear as definitions that nothing references.
  Treat the generated schema as a lower bound on the protocol, never as a
  complete picture — reading it as complete leads to "dynamic tools do not
  exist". Answers about experimental surface come from `openai/codex` at the
  matching git tag (`rust-v0.146.0`). Full sourcing in
  `tidepool-agent/fixtures/app-server-0.146.0/PROTOCOL-NOTES.md`.

  Same cause, second consequence: `codex-codes` 0.146.4 exposes none of the
  dynamic-tool types either. They are hand-rolled under
  `backend::codex::dynamic_tools` and sent through the crate's raw `request()`
  escape hatch — which PRD 18 anticipated, and which keeps containment intact.

- `outputSchema` constrains the **text of the final `agentMessage` item**.
  There is no separate structured-output field on the turn or the thread item:
  the model's final message text *is* the schema-conforming JSON, and the
  driver decodes it. This matters more than it looks — see the encoding
  polarity note below.

Schema regenerate command (idempotent, offline, safe):

```bash
codex app-server generate-json-schema --out tidepool-agent/fixtures/app-server-0.146.0/
```

## Encoding polarity — the seam between the two wave-1 gates (WAVE-2 DESIGN ITEM)

Wave 1's two Haskell/Rust results are each correct and together they expose a
constraint neither lane could see alone. Naming it here so the vertical core
does not discover it by writing the wrong encoder first.

**What gate 1(b) proved** (`receipt-structural-codec.md`): lists and genuine
recursion survive the real extract/JIT. The self-referential dictionary
(`Structural Plan` → `Structural [Plan]` → `Structural Plan`) elaborates and
runs. That is the gate's actual question and the answer is GO, independent of
wire shape. It uses one uniform `{"tag": …, "fields": [positional…]}` shape for
every constructor form — a deliberate, well-argued response to the three-shapes
mistake in `../codex-review-2026-08-08.md` item 8.

**What the backend requires** (`receipt-adapter-bringup.md`): the model is the
encoder on the other side of two boundaries, and it knows only a JSON Schema.
Observed live, the child emitted tool arguments as a **named-field object**
(`{"question": "What is the secret passphrase?"}`), and `outputSchema`
constrains the final message text the model itself writes.

So the structural interpreter has two boundaries with genuinely different
requirements, and one encoding cannot serve both:

| Boundary | Who encodes / decodes | Requirement |
|---|---|---|
| Tidepool ↔ Tidepool (authored messages, internal state) | Tidepool both ends | Uniformity is a virtue. `{"tag","fields"}` is fine, and inverse-by-construction is the property that matters. |
| Tidepool ↔ model (tool inputs, tool outputs the child reads, terminal results) | the **model** on the far side | Must be named-field JSON **describable by a JSON Schema**. Positional `fields` arrays are not something a model can be asked to produce reliably, and field order is not a contract the model ever sees. |

Consequences to settle in the vertical core, not now:

1. A sum type crossing the model boundary needs a JSON-Schema-expressible
   discriminated shape (`oneOf` + a `const` tag, or a tag field), not a
   positional array. Whether every authored result type may be a sum, or only
   records, is a real authored-surface decision.
2. Schema emission and encoder must come from the SAME traversal for the
   model-facing direction too — the drift argument PRD 18 makes for
   `compileTools` applies with more force here, because a schema/encoder
   disagreement shows up as a model producing well-formed JSON we then reject.
3. Field names become load-bearing on the model-facing side, so
   selector→wire-name normalization applies to record fields, not just tool
   names.

None of this reopens gate 1(b). The recursion/list result is what transfers;
the wire shape was scoped to a proof and said so.

## PRD 18 was revised on root's tip AFTER this wave forked

Root's tip (`harness-interaction-surface` @ `4eb9283b`) carries a substantial
PRD 18 revision. **Design against the tip's text, not this branch's copy**,
for anything touching agent operations or lifecycle. Nothing wave 1 landed is
invalidated — the Servant eDSL and diagnostics sections are byte-identical
across the revision, and the adapter and codec results are untouched by it.

The deltas that change vertical-core design:

- **Agents may continue running between resident cycles.** The cycle boundary
  now requires *Haskell-continuation* quiescence, not *agent* quiescence. A
  `waitAgent` continuation must resolve before the cycle ends; an independently
  running agent may outlive it. This supersedes the earlier "make every worker
  quiescent and release it" framing.
- **`AgentReference` + `attachAgent`.** A stable `AgentId` plus a protocol
  fingerprint is checkpointable resident data; a later cycle recreates the
  typed handle from it and fails loudly if the deployed protocol no longer
  matches. The registry gains a first-class detached-but-running state.
- **Every control operation collapses into one `pokeAgent` with a TAGGED
  message.** `sendMessage`, `followupTask` and `interruptAgent` are all gone as
  authored operations. The entire authored control surface is `spawnAgent`,
  `pokeAgent`, `waitAgent`, **plus the tag choice**:

  ```haskell
  whenSafe     :: input -> Poke input
  interrupting :: input -> Poke input
  ```

  `whenSafe` steers an active turn when possible, starts or queues a follow-up
  when idle, and — open decision — either reaches or waits out a parked tool
  call. `interrupting` cancels any active turn and delivers its message as the
  next turn. Delivery is durable in both cases; only a terminal or released
  target produces a typed failure.
- **`AgentFinished` splits into `AgentWentIdle` and `AgentFinalized`.** Idle is
  an ordinary typed outcome, not an exception, and it is what makes the
  escalation ladder authorable: `whenSafe (PleaseFinalize …)` first,
  `interrupting` later, resident policy choosing when to climb. The runtime
  supplies liveness and staleness; it never escalates on its own.
- **`drainMailbox`** — the resident's own typed inbox, drained atomically once
  per cycle rather than polled.
- **Endgame note:** dev-tree replaces the Exomonad swarm for Tidepool's own
  development and drives headless agents directly through this adapter. No
  Exomonad machinery is ported.

**RULED (root, 2026-08-08, tip `930f326e`): PRD 18 supersedes.** PRD 19's poke
paragraph had contradicted it — "no runtime delivery queue and no auto-enqueue
on idle agents; PRD 18's message semantics stand unmodified" — while naming
both operations the revision deleted. Rewritten on root's tip: pokes are
`pokeAgent`'s durable per-agent queue, retained until deliverable, never
silently discarded, and idle delivery starts or queues a follow-up.
**Residents own reaction policy; the runtime owns delivery.**

**"Fire-and-forget" is still live — it describes the SENDER, not delivery.**
(Inanna, via root, 2026-08-08.) The decided model is *fire-and-forget poke plus
wait for response*, and both halves hold simultaneously:

- **Sender's contract — fire-and-forget.** `pokeAgent` is non-blocking and
  carries **no per-message response guarantee**. A poke is not a request/reply
  pair.
- **Delivery — durable.** Steer if active, start or queue a follow-up if idle,
  retain if temporarily unsteerable, never silently drop.

**Therefore: do not build per-poke reply correlation.** There is no poke id to
match a response against. Responses flow through `waitAgent` outcomes and the
typed resident inbox. A registry that grows a poke→reply map has misread this.

What PRD 19 actually got wrong was narrower than "fire-and-forget": it asserted
*no runtime delivery queue and no auto-enqueue on idle agents*, which is a
delivery claim, and it froze a description of the pre-revision system into a
standing prohibition. The failure mode is worth keeping because it recurs — a
sentence characterizing how things currently work is not a decision that they
must keep working that way, and the two are easy to confuse once the sentence
sits in a locked-decisions section.

Two further specifics for the registry API:

- **`drainMailbox @ResidentMessage` arrival SCHEDULES A CYCLE.** The inbox is
  durable and typed, and its arrival is a *driver* integration point: the
  registry exposes "mail arrived" as a wakeup signal the driver consumes. The
  resident never polls; it drains once per cycle.
- **Observations out, no built-in escalation.** The runtime exposes liveness
  and staleness observations. Poke-again, replace, escalate to the operator, or
  stop is resident **policy**. Keep that boundary clean — the registry reports
  that a worker is stale; it never decides what to do about it.

### Spike 2, now precisely scoped — the wave-2 adapter work item

The tagged-poke surface splits the old "steer/interrupt while parked" question
into two, with **different stakes**. Wave 2 must answer them separately; a
combined verdict hides the one that matters.

| Question | If the answer is no |
|---|---|
| Does a `whenSafe` poke reach a turn parked on a dynamic tool call, or wait it out? | **Acceptable degradation.** Delayed delivery is fine — the queue retains it and delivers after the tool call resolves. Record which happens; the API does not narrow either way. |
| Does a parked request **block** an `interrupting` poke? | **Needs an adapter workaround**, which PRD 18 names as required before broader implementation. An `interrupting` poke that cannot land while a child is parked breaks the escalation ladder at exactly the point a resident reaches for it — a stuck worker is usually stuck *in* a tool call. |

The second is the one to design the run around. The first is a measurement.

Both are wave-2 scoped; neither was probed during wave 1's single gated turn,
by the go's own condition against park-duration probing.

## HOLD lines (root announces each lift; all intact as of wave 1)

1. **generic-surface's fold** — no consumption of their Generic metadata
   utilities, no `16-generic-spike-receipts.md`, no `Harness.Prelude`
   integration, and no edit to `haskell/lib/Tidepool/Form.hs`, their Generic
   substrate, or `tidepool-mcp/src/preamble.rs`. All agent-wave Haskell is NEW
   files until then.
2. **worktree-wave's vertical core** — the coupled-spawn seam is designed
   JOINTLY, via root, when both sides are ready. Until then `seam::Workspace`
   is transitional data and every use site says so.

   **Text-stability part LIFTED (root, tip `d766b9cb`):** the PRD 19 rewrite
   has landed and both PRDs are final. Design *inputs* are now stable and
   confirmed: `WorkerRun` as the coupled-spawn result shape, one worktree per
   agent with explicit second-binding failure, `worktreeHead` in the public
   surface, `readOnlyOf` gone.

   **The hold itself still stands.** Stable text is not the announcement —
   this hold was always on worktree-wave's vertical core existing and the seam
   being designed JOINTLY via root, and neither has happened. `seam::Workspace`
   stays transitional.

   Worth noting for whoever designs it: `worktreeHead` exists so a resident can
   compare against a checkpointed head before re-registering handlers, which is
   what closes the between-cycle no-replay gap. That is a direct consequence of
   agents outliving cycles — subscriptions do not survive, so a resident
   re-registering in a later cycle would otherwise silently miss everything
   that moved while it was away.
3. **root's realm step-4 go-signal** — nothing in `resident.rs`
   pending/`ChildSuspended`.

**Lifted (root, 2026-08-08):** the registry/lifecycle design, to the extent it
was blocked on the poke contradiction above. Detached-but-running agents,
`attachAgent` by `AgentReference` + protocol fingerprint, `AgentWentIdle` as an
ordinary outcome distinct from `AgentFinalized`, Haskell-continuation
quiescence at cycle boundaries, and `drainMailbox` are all green against the
tip's semantics. Holds 1 and 2 above still stand and still bound this work:
substrate consumption waits on generic-surface's fold, and the coupled-spawn
seam waits on the joint announcement.

The realm parking machinery is consumed ONLY through
[`../realm-lanes/continuation-parking-contract.md`](../realm-lanes/continuation-parking-contract.md),
never by reading `jit_machine.rs`. Its consumer guidance is binding: derive the
declared handled prefix from the same value that constructed the handler stack,
never re-declare it at a dispatch site.

## Naming collision to resolve before the vertical core

`Tidepool.Agent` is already taken — `haskell/lib/Tidepool/Agent.hs` is the
harness answerer's capability row (`Eff '[AskUser, Fork, Finalize]`), unrelated
to PRD 18. PRD 18 asks for the public surface at `Tidepool.Agent`. Wave 1 sits
under `Tidepool.Agent.*` (legal alongside the existing module) and does not
rename anything. The rename-or-relocate decision is root's, taken with the
harness owner, not a lane's to make unilaterally.
