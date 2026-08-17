# tidepool-agent — typed headless subagents (PRD 18)

The ONLY crate in the workspace that knows a coding backend exists. See the
repo-root `CLAUDE.md` for the project map, `tidepool-handlers/CLAUDE.md` for
the effect-handler side, and
`plans/self-iterating-harness/18-typed-subagent-spawning-prd.md` for the design
authority.

## The containment boundary

Two rules make it structural rather than aspirational:

1. `codex-codes`, app-server JSON-RPC types, and the word "Codex" appear ONLY
   under `src/backend/codex/`. Everything else speaks `src/seam.rs`.
2. Nothing in `seam.rs` may be DEFINED in terms of a backend type. A seam type
   that is a re-export or newtype of a `codex-codes` type has already broken
   the boundary, because a backend version bump then reaches the Haskell
   surface.

The one place this is deliberately bent, and why: `AgentBackend::transcript_jsonl`
returns `Vec<String>`, not typed frames. A recording has to cross the seam so a
live run can commit its own fixture, and opaque lines are how it crosses
without the seam naming a protocol type.

## The seam is a STEP function, not run-to-completion

```rust
trait AgentBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, _>;
    fn start_turn(&mut self, thread: &BackendThreadId, spec: &CycleSpec) -> Result<TurnEvent, _>;
    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, _>;
}
```

```text
start_turn ──► ToolCall ──► (the parent's Haskell handler runs) ──► resume ──► …
       └────► Completed                                              └────► Completed
```

**Why it is a step function.** A tool call has to be answered by the
PARENT's handler, and a
parent handler is authored Haskell (`Tidepool.Agent.Contract`'s `Tool` carries
`handler :: input -> m output`). No Rust closure can run one:
`EffectHandler::handle` has no machine handle, the machine is already
`&mut`-borrowed at the dispatch site, and a Haskell closure cannot even reach a
handler as data (`heap_bridge.rs`'s `ClosurePolicy` rejects `TAG_CLOSURE` or
substitutes `CLOSURE_SENTINEL`). So the loop lives in Haskell
(`Tidepool.Agent.Spawn.spawnAgentWithTools`) and this seam is what it steps.

Between a `ToolCall` and its `resume`, the child's JSON-RPC request is simply
UNANSWERED. That costs nothing and bounds nothing, which is what lets the
parent take as long as answering honestly requires. The flip side: a caller
that abandons a parked call leaves the child parked until the backend's own
timeout, and the only thing bounding that is ownership — dropping the backend
takes the child down.

`run_turn_to_completion` and `CoupledSpawner::spawn_one_cycle` are COMBINATORS
over the step surface, not second primitives. That is PRD 18's own rule for
synchronous delegation, and it is why the no-tools path cannot drift from the
tools path.

## Concurrency: a shared substrate, N detachable sagas

`spawn.rs` splits into two pieces because the two halves of a spawn have
opposite ownership:

- **`SpawnSubstrate`** — SHARED, behind one `Arc<Mutex<…>>`. Holds the
  `WorktreeManager`, the flocked `BindingTable`, and the agent-id counter.
  Single-owner is not a policy choice: `BindingTable::open` takes a lifetime
  flock on its binding root, so "one substrate per cycle" does not exist.
- **`CycleSaga`** — PER-CYCLE, carrying its own `Arc` to that substrate, so it
  can be moved to another thread and driven there.

**The load-bearing invariant: no code path holds the substrate lock across a
backend call.** A saga that does serializes every cycle behind one model turn
and silently reinstates the one-agent-at-a-time constraint this design removed.
It is enforced by shape, not only by prose: every critical section goes through
`lock_substrate` (or a bare `substrate.lock()` in `abandon`/`roll_back`), and
none of those bodies contains a `backend.` call. Grep `lock_substrate` to audit
it.

`resolve_workspace` is inside the lock even though `WorktreeManager` is
internally immutable — concurrent `git worktree add` against one source
repository contends on git's own locks, and a spurious `GitFailure` from that
would read as a real allocation failure.

A POISONED substrate mutex means another cycle panicked mid-saga, so the
binding table's in-memory rows may not match disk. It surfaces as a loud
`SpawnError::Binding` / `WorktreeError::StorageFailure` naming the poisoning —
never `unwrap()` (one cycle's panic must not become every cycle's) and never
`PoisonError::into_inner` (writing binding rows on top of unknown state). It is
spelled with existing variants because `SpawnError`'s variants are the wire
contract `tidepool-handlers` converts exhaustively.

`spawn_one_cycle`, `CoupledSpawner::begin`/`answer`, and the detached path are
all COMBINATORS over one `CycleSaga` — same rule as `run_turn_to_completion`
over the step seam.

## Cancellation: reap first, then settle

`AgentBackend::canceller()` hands out a `Send + Sync` `BackendCanceller` that
reaps the backend FROM ANOTHER THREAD. Take it BEFORE the cycle runs: a cycle
thread inside `start_turn` holds `&mut` on the backend, so nothing else can
reach it. A flag the blocked thread would have to check is not cancellation.

- `CodexCanceller` SIGKILLs the app-server child through a **pidfd**, never a
  bare numeric pid. It cannot go through `Session::shutdown` (that consumes
  `self` and needs the runtime the blocked thread is holding), so a pidfd is
  opened (`pidfd_open`, via `libc::syscall` — the crate ships the syscall
  number but no typed wrapper) into a shared `Arc<Mutex<PidFdSlot>>` the
  moment the session connects.

  **A pid is not a durable name for a process, and "we spawned it" is not what
  makes it safe to signal.** Once the child is reaped — by its owning `Child`
  on drop, or by tokio's SIGCHLD reaper while the backend is still alive — the
  kernel is free to hand the number to anyone, and on this box "anyone" is
  plausibly the operator's own Codex session. A numeric-pid design can only
  narrow that window (re-check identity immediately before `kill`, still two
  racing syscalls); a pidfd removes it structurally: `pidfd_open` binds the fd
  to the exact process INSTANCE, not to its pid number, so `pidfd_send_signal`
  against it fails `ESRCH` forever once that instance is reaped — including if
  the kernel later hands the same number to a brand-new, live process. There is
  no numeric-pid fallback anywhere in this path: if `pidfd_open` fails (an old
  kernel, `EMFILE`, the process already reaped before the fd could be opened),
  the slot records `PidFdSlot::IdentityUnprovable` and cancellation against
  that backend FAILS CLOSED — it signals nothing, ever, rather than falling
  back to the pid.

  `Drop for CodexAgentBackend` still clears the slot (closing the fd) before
  the struct's fields — and therefore the `Child` — drop, so a cancel racing a
  just-completed cycle is a prompt, observable no-op rather than a signal
  against an fd about to be reclaimed anyway. That ordering is no longer the
  sole safety mechanism the way a numeric-pid design needs it to be (the
  pidfd's own semantics already forbid reuse), but it stays for promptness.

  Pinned by named rows in `driver.rs`'s `mod tests`: a dropped backend leaves
  its cancellers inert, a cancel during the connect window is recorded without
  a pidfd to signal, and a pidfd that could not be acquired for an
  already-reaped process leaves cancellation `IdentityUnprovable` — signalling
  nothing, never falling back to a bare pid.
- The default is a no-op canceller, correct for a backend with no process
  (`replay`, any in-process one). It is NOT a placeholder for an unimplemented
  one on a backend that owns a process — a canceller that returns without
  reaping is worse than none, because the supervisor then joins a thread that
  never returns.

**Order: kill, THEN settle.** `CycleSaga::abandon()` takes the substrate mutex
briefly to settle the binding `Released`; settling first would hold the lock
across a reap of unknown duration. `abandon` is IDEMPOTENT — on a saga that
already completed, rolled back, or was abandoned it is `Ok(())` writing
nothing, because a cancel racing a completion is a real sequence and must not
write two lease rows for a life that ended once. Retain-first is locked
(`tidepool-worktree/CLAUDE.md`): cancellation settles and deletes nothing.

## Per-cycle backends: `AgentBackendFactory`

Concurrent cycles never share a backend — an `AgentBackend` is a step function
over ONE live thread, and two cycles sharing one would interleave their
`resume`s onto the same session. `CodexBackendFactory` makes one instance per
cycle; `ClosureBackendFactory` adapts a closure.

Config isolation holds for N instances exactly as for one, and the reason is
structural: `cwd` rides `turn/start` and `ThreadStartWithDynamicTools` has no
`cwd` field at all, so the project-trust write is unreachable from every
instance independently. `new()` shares nothing — fresh runtime, no statics, the
model catalogue cached PER BACKEND. What N instances do share is the operator's
real `~/.codex`, read-only in the normal path; the one writable case is a
credential refresh of `auth.json`, which is a property of running `codex` at
all and which `ConfigSnapshot` already fails loudly on, at one instance or at
eight.

## Model policy: allowlists, never denylists

Each `ModelPolicy` names an ordered allowlist (`driver::preference_for`);
resolution takes the first slug `model/list` actually offers and FAILS
otherwise, naming what was available.

| policy | allowlist |
|---|---|
| `CheapPlumbing` | `gpt-5.4-mini`, then `gpt-5.6-luna` |
| `CheapestGpt56` | `gpt-5.6-luna` — and nothing else |

A banned model is unreachable **by construction**, not by a skip-branch a
future slug could slip past. `CheapestGpt56` exists because a budget grant
named that exact tier: `CheapPlumbing` would have resolved to the cheaper
`gpt-5.4-mini`, and **cheaper is not the same as granted**.

`ReasoningEffort` is a separate axis (which engine vs. how much of it) and is
a CLOSED enum on the seam even though the wire type is an open string newtype —
callers choose among efforts Tidepool supports, not among whatever a server
advertises.

## Testing: mocks implement the SEAM, recordings carry the PROTOCOL

Standing policy (root/human, 2026-08-11). Three tiers, and which one a test
belongs in is not a matter of taste:

- **`backend::mock::MockBackend`** — a DUMB seam implementation. A scripted
  list of `TurnEvent`s in, a record of every reply out. Zero protocol
  semantics: no JSON-RPC, no frame ordering, no session lifecycle, no error
  shapes. **If a test can only pass by teaching this type protocol behavior,
  the test is wrong** — either the seam is in the wrong place, or the test
  wants a recording.
- **`backend::codex::replay`** — a `TranscriptTransport` that feeds the REAL
  `Session` pump real recorded JSONL frames. This is where protocol behavior is
  proven: park/reply/resume, correlation, the `success:false` shape,
  `turn/completed` projection, usage capture. Fast tier, no process, no tokens.
  A recording is evidence; a hand-written imitation is drift waiting to happen.
- **Live** — never in a suite. `tidepool-handlers/examples/live_tool_loop.rs`,
  double-gated on a credential and `TIDEPOOL_AGENT_LIVE=1`, run by hand.

The three `#[ignore]`d live tests in `backend::codex::process` keep their
attributes forever.

## `outputSchema` is not arbitrary JSON Schema

The CLI's own doc comment says it is. It is not — see
`fixtures/app-server-0.146.0/PROTOCOL-NOTES.md` §5. The schema is forwarded to
the model API's structured-output `response_format`, whose ROOT must be an
object, so a sum-typed result (which renders a root `oneOf`) is refused at
request validation. Model alternatives as a FIELD. Tool INPUT schemas go
through a different field with a different validator and are unaffected.

Generalizing: **treat the generated schema and the CLI's doc comments as a
lower bound on the protocol, never as a complete or accurate picture.** This is
the second time that has bitten — the first was `dynamicTools` appearing absent
because schema generation silently drops `#[experimental]`-gated fields.

## Config isolation is a first-class result

No normal worker run may mutate the operator's `~/.codex` (PRD 18 acceptance
criterion 11). The adapter runs against the operator's REAL Codex home on
purpose — proving that is the point, not something to route around by copying
credentials into an isolated `CODEX_HOME`, which PRD 18 forbids. The shape that
avoids the documented project-trust write is `cwd` at TURN start and never at
thread start; `ThreadStartWithDynamicTools` has no `cwd` field at all, so it is
structural. `isolation::ConfigSnapshot` checks it per run, and a run that
succeeded while mutating the config is a FAILURE.

## Pinning

Codex CLI **0.146.0** + `codex-codes` **=0.146.4**, pinned TOGETHER. Dynamic
tools are an experimental surface, so a version bump is a fixture re-run, not a
lockfile refresh. `codex-codes` exposes none of the dynamic-tool types (see
above); they are hand-rolled in `backend::codex::dynamic_tools` and sent
through the crate's raw `request()` escape hatch.

Regenerate the protocol schema (offline, idempotent, does not touch `~/.codex`):

```bash
codex app-server generate-json-schema --out tidepool-agent/fixtures/app-server-0.146.0/
```
