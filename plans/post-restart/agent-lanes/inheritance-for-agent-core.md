# Inheritance — agent-wave → agent-core

Written at agent-wave's checkpoint fold for the successor lane (`agent-core`,
Chain B of `plans/post-restart/overnight-plan.md`), which runs on the tip after
both agent-wave and worktree-wave fold. This is what a fresh lane needs to not
redo, not rediscover, and not walk into.

Read alongside `README.md` (lane index, standing rules) and the three receipts
in this directory.

---

## 1. What is already real — do not rebuild it

| Thing | Where | State |
|---|---|---|
| Backend containment crate | `tidepool-agent/` | Compiles; `seam.rs` is the Tidepool vocabulary, `backend/codex/` the only place backend types may appear |
| Config-isolation checker | `tidepool-agent/src/backend/codex/isolation.rs` | Committed test helper. Reusable — do NOT re-derive it |
| Live app-server driver | `backend::codex::process` | Raw JSONL transport, every frame capturable |
| Hand-rolled dynamic-tool types | `backend::codex::dynamic_tools` | Necessary — see §3 |
| Protocol schema fixture | `tidepool-agent/fixtures/app-server-0.146.0/` | Version-matched, regenerable offline, free |
| Live-turn transcript | `fixtures/app-server-0.146.0/phase4-live-turn.jsonl` | 35 frames, the whole vertical |
| Structural codec (list + recursion) | `haskell/lib/Tidepool/Agent/CodecSpike.hs` | Proven on the real JIT; see §5 for its scope limit |
| eDSL contract algebra | `haskell/lib/Tidepool/Agent/Contract.hs` | See `receipt-mode-encoding.md` for the gate verdict |

**The vertical core has already run once end to end**: task → `item/tool/call`
→ Rust reply → same turn resumed → structured completion decoded. Chain B's job
is to route that through the Haskell handler and the realm, not to re-establish
that the backend can do it.

## 2. Pinned versions, and what pinning means here

- **Codex CLI `0.146.0`** — `codex --version` → `codex-cli 0.146.0`.
- **`codex-codes` `=0.146.4`** — exact pin. The patch skew against the CLI is
  deliberate and recorded; attribute any protocol mismatch here first.

Dynamic tools are an experimental surface. A version bump means regenerating
the fixture directory and re-running the adapter's compatibility tests — not
refreshing a lockfile.

## 3. The three findings that will cost you a day if you rediscover them

**(a) The generated protocol schema is a LOWER BOUND, not a picture of the
protocol.** `codex app-server generate-json-schema` silently drops
`#[experimental(...)]`-gated fields. That is why `DynamicToolSpec` appears as a
definition nothing references, and why `dynamicTools` looks absent from
`ThreadStartParams`. It is not absent: it is a top-level field of
`ThreadStartParams`, sibling to `cwd`/`config`/`model`, thread-scoped, unlocked
by `InitializeParams.capabilities.experimentalApi = true`. Source experimental
surface from `openai/codex` at the matching git tag (`rust-v0.146.0`), never
from the schema alone.

**(b) `codex-codes` 0.146.4 exposes none of the dynamic-tool surface** — same
root cause. Hence the hand-rolled types plus the crate's raw `request()` escape
hatch. This is not a workaround to clean up; it is the supported path, and PRD
18 named it.

*Open decision, deliberately left to root:* what `codex-codes` is still buying.
It supplies process lifecycle, JSONL framing, stderr drain, JSON-RPC envelope
types, and generated types for the stable surface — but nothing on the critical
path, and no correlation/dispatch logic this crate calls. PRD 18's rule is
"vendor or replace at the first material protocol or maintenance problem".
The evidence is in `PROTOCOL-NOTES.md`; the decision is not agent-core's to
take unilaterally.

**(c) `~/.codex` cannot be diffed wholesale.** It holds live sqlite
(`logs_2`/`goals_1`/`memories_1` plus `-wal`/`-shm`) that the operator's own
sessions write continuously, and `codex app-server` starting creates sidecars
for already-existing databases. A naive whole-directory diff produces false
positives. The committed checker scopes to `config.toml`/`auth.json`/
`installation_id` sha256s plus a top-level listing diff, excluding sidecars for
pre-existing `.sqlite` files while still flagging one for a genuinely new
database. Use it; don't rewrite it.

## 4. Config isolation is an acceptance criterion, not a nicety

PRD 18 criterion 11. Held across offline, token-free-live, and token-spending-
live runs, independently re-verified outside the test process.

The specific mutation being avoided is the **project-trust write**: omitting
`cwd` from `thread/start` and supplying it at `turn/start` kept
`config.toml` byte-identical, with no `projects.<tempdir>` entry appearing.
Keep that request shape. If a future change needs `cwd` at thread start, that
is an isolation regression to escalate, not to absorb.

Never copy or rewrite ChatGPT credentials into an isolated `CODEX_HOME` to work
around this — PRD 18 forbids it; it needs a separately designed credential
boundary.

## 5. The encoding-polarity trap — the most likely way agent-core goes wrong

Fully described in `README.md` § "Encoding polarity". The short version:

- Gate 1(b) proved **lists and recursion survive the JIT**. That result
  transfers unconditionally and is the point of the gate.
- Its wire shape — uniform `{"tag", "fields": [positional]}` — is correct for
  **Tidepool ↔ Tidepool** and is deliberately scoped to the proof.
- It is **wrong for Tidepool ↔ model**. The child emits tool arguments as
  named-field objects (observed live: `{"question": "..."}`), and
  `outputSchema` constrains the final message text the model itself writes.
  On that boundary the far side knows only a JSON Schema, and a positional
  array is not something a model can be asked to produce.

So a sum type crossing the model boundary needs a JSON-Schema-expressible
discriminated shape, schema and encoder must come from one traversal in that
direction too, and selector→wire-name normalization applies to record fields,
not just tool names.

Also note (from bring-up): `outputSchema` has **no separate structured-output
field** — the final `agentMessage` *text* is the schema-conforming JSON.

## 6. Registry/lifecycle semantics — design against the FINAL PRDs

Root's tip `d766b9cb` is authoritative for PRDs 18 and 19. The deltas from
earlier drafts, all confirmed:

- **Agents may run between resident cycles.** The boundary requires
  *Haskell-continuation* quiescence, not agent quiescence.
- **`AgentReference` + `attachAgent`** — stable id plus protocol fingerprint is
  checkpointable data; reattach fails loudly on protocol mismatch.
- **Tagged pokes are the whole control surface.** `spawnAgent`, `pokeAgent`,
  `waitAgent`, plus `whenSafe` / `interrupting`. There is no separate
  `sendMessage`, `followupTask`, or `interruptAgent`.
- **Fire-and-forget is the SENDER's half; the durable queue is DELIVERY.** Both
  hold at once. **Do not build per-poke reply correlation** — there is no poke
  id to match against. Responses come via `waitAgent` outcomes and the inbox.
- **`AgentWentIdle` vs `AgentFinalized`** — idle is an ordinary outcome and is
  what makes the escalation ladder authorable (`whenSafe (PleaseFinalize …)`
  first, `interrupting` later).
- **`drainMailbox` arrival SCHEDULES A CYCLE** — a driver wakeup signal, not
  something the resident polls.
- **Observations out, policy in.** The runtime reports liveness/staleness; it
  never decides to poke, replace, escalate, or stop.

Realm parking is consumed ONLY through
`../realm-lanes/continuation-parking-contract.md`. Its binding consumer rule:
**derive the declared handled prefix from the same value that constructed the
handler stack**, never re-declare it at a dispatch site. Under that discipline
the lying-realm residual is unconstructible rather than merely unlikely.

## 7. Spike checklist — answered vs open

| Spike | State |
|---|---|
| 1 — parked typed tool | **Answered live.** Park/reply/same-turn-resume confirmed against the real server. Park duration exercised: ~0 ms (driver replied synchronously) — a floor for a ladder, not a duration result |
| 2 — steer/interrupt while parked | **OPEN.** Now two questions with different stakes — see below |
| 3 — concurrency / one-server ownership | **OPEN.** Single-agent only so far. The correlation triple (`threadId`/`turnId`/`callId`) is already flowing, so this is proving it under contention, not discovering it |
| 4 — structured completion | **PARTIAL.** Success path confirmed end to end. Malformed/rejected results and the `finish_task` comparison are open |
| 5 — configuration isolation | **Answered definitively** (§4) |

**Spike 2 splits, and a combined verdict would hide the one that matters:**

- *Does a `whenSafe` poke reach a turn parked on a tool call, or wait it out?*
  Either is fine — delayed delivery is acceptable degradation, the queue
  retains it. This is a measurement.
- *Does a parked request **block** an `interrupting` poke?* If yes, PRD 18
  requires an adapter workaround before broader implementation. **Design the
  run around this one** — an `interrupting` poke that cannot land while a child
  is parked breaks the escalation ladder exactly where a resident reaches for
  it, because a stuck worker is usually stuck *in* a tool call.

## 8. Gotchas a successor will otherwise hit

- **`Tidepool.Agent` is already taken.** `haskell/lib/Tidepool/Agent.hs` is the
  harness answerer's capability row (`Eff '[AskUser, Fork, Finalize]`),
  unrelated to PRD 18. This wave sat under `Tidepool.Agent.*` rather than
  renaming. PRD 18 wants the public surface at `Tidepool.Agent` — that rename
  is root's call with the harness owner, not a lane's.
- **`seam::Workspace` is TRANSITIONAL** and marked so at its definition. PRD 19
  couples agent creation to worktree allocation: `WorkerRun` as the result
  shape, one worktree per agent, second binding fails explicitly, rebind only
  after terminal/release, `readOnlyOf` gone. `worktreeHead` exists so a
  resident can compare against a checkpointed head before re-registering
  handlers — that is what closes the between-cycle no-replay gap, which only
  exists because agents outlive cycles while subscriptions do not.
- **The vendored `ToJSON`/`FromJSON` generic defaults reject any sum with a
  non-nullary constructor** ("single-constructor records only"). Neither
  `WorkerResult` nor `Plan` could derive through them. That is why the codec is
  hand-rolled on base `GHC.Generics`.

  **RE-VERIFY THIS BEFORE RELYING ON IT.** It was true when measured (see
  `receipt-structural-codec.md`, which read
  `haskell/lib/Tidepool/Aeson/Value.hs` and `FromJSON.hs` directly), but as of
  2026-08-09 `generic_deriving_337::sum_type_rejected_at_compile_time` is a
  sanctioned inherited red — and that test asserts precisely that
  `data S = A Int | B Int deriving (Generic, FromJSON)` must NOT compile. Its
  being red means either sum types now *do* derive through the vendored
  default, or they still fail and only the diagnostic text moved. **Which of
  those is true was not established here**, and the two have opposite
  consequences: under the first, the hand-rolled codec's central rationale is
  stale and the vendored path may be usable; under the second, nothing changes.
  Read the current state of that machinery rather than either receipt.

  Note this does NOT touch gate 1(b)'s result. That gate asked whether lists
  and recursion survive the real JIT, and the answer transfers regardless of
  which codec you build on.
- **Model skew in the fixtures.** `phase4-live-turn.jsonl` was recorded with
  **`gpt-5.6-terra`**. Overnight policy is **`gpt-5.4-mini` only** (see §9), so
  a fresh run will not reproduce that transcript turn-for-turn — a weaker model
  may need a blunter prompt to reliably reach for the tool. Treat the fixture
  as protocol truth, not as behavioral baseline.

## 9. Operational rules in force at handoff

Time-stamped, because some are incident responses that may be lifted — check
with root rather than assuming they still bind.

- **Codex model policy (Inanna, binding for overnight runs, 2026-08-09):**
  **`gpt-5.4-mini` ONLY.** Cheap plumbing tier — no spend anxiety, but do not
  waste it. **NEVER `gpt-5.6-terra` overnight.** Ephemeral threads, small
  synthetic tasks, temp workspaces, and **stop-and-hold on anything anomalous**
  (auth prompts, config mutation, rate-limit walls).
- **Broker every slot-taking run** through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh detach -- <cmd>` — absolute
  path (the parent script sees 6 slots; a worktree copy sees 3), never
  `exclusive`, **one brokered leg at a time**, drain rather than kill.
- **Per-run `ghc-heavy` cap is 1** (`fc3363dc`). Box ceiling is slots × cap.
- **`--no-fail-fast` explicitly** until the worktree carries `7d57cea5`;
  report completed-vs-selected with the filter named.
- **Receipts:** specific guards pass BY NAME with their own pass line; name the
  INSTRUMENT beside every number; cross-lane guards name the base commit and
  sit inside the guarded lane's gate set.
- Commit `--no-verify`; never `git add -A`; repo-root `tmp/` protected.
