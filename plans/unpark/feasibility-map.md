# Unparking the repl: feasibility map

Question: can `tidepool-repl` drop its parked-OS-thread suspension and run on
the threadless (stow-as-data) engine the harness already uses?

**Verdict: yes. No mechanism conflict.** What blocks a naive cutover is not a
repl behavior that *requires* a live thread mid-suspension — it is two missing
pieces of plumbing, both additive:

1. Two of the JIT machine's four result-materialization policies have no
   suspendable sibling (`Project` = multi-bind, `Render` = the bare-expression
   `it` path). The bare-expression path is exactly what `tests/ask_resume.rs`
   exercises, so this is required, not optional.
2. The repl suspends **mid-Rust-stack**, inside `run_block`'s item loop. The
   parked thread is what currently holds that loop state. Threadless requires
   the loop state — and each item path's post-run tail — to become stowable
   data.

Neither is a semantic obstacle. Both are mechanical.

---

## 1. Where the parked thread is load-bearing today

Every code path below exists BECAUSE the suspension is a blocked OS thread.

### 1.1 The dispatcher park (`tidepool-repl/src/ask.rs`)

`ReplAskDispatcher` is a `DispatchEffect` wrapper. On a tag `>= ask_tag` it
sends `WorkerMessage::Suspended` and calls `response_rx.recv()` — a synchronous
block on `std::sync::mpsc`. The whole native stack below it (the JIT effect
loop, `run_bind`, `run_one_item`, `run_block`, `worker_loop`) stays live for
the duration of the suspension.

Deleted wholesale: `ReplAskDispatcher`, `WorkerMessage`, `ResumeMsg`,
`extract_ask_request` (the engine already has an identical one).

### 1.2 The worker thread (`tidepool-repl/src/worker.rs`)

One dedicated OS thread per session, holding the `Session` for its whole life,
fed `WorkerJob`s over an mpsc channel. Two jobs in one:

- **serialization** — one consumer, so turns are sequential by construction;
- **pinning** — the `Session` was assumed thread-pinned.

The pinning half is obsolete: `PersistentSession` is already `Send`
(`unsafe impl Send` on `JitEffectMachine` + `BindingTable`), which is why the
harness moves `ResidentSession` across `spawn_blocking` freely. The
serialization half is replaced by the harness's checkout discipline
(`Idle | Running | Suspended{…}` under one short lock).

Deleted: `WorkerJob`, `WorkerHandle`, `spawn_worker`, `worker_loop`,
`drain_with_error`, `dead_sender`, and the detach-vs-join dance that exists
only because a parked/runaway thread must not be joined.

### 1.3 The suspension payload (`tidepool-repl/src/state.rs`)

`Suspension` carries `response_tx` + `session_rx` — the two channel ends
addressing the parked thread. Under threadless the payload is the *session
itself* (holding the stowed continuation) plus `cont_id`/`expected_schema`.
The `SessionState` enum and its never-hold-the-lock-across-await invariant
survive unchanged; only the payload shrinks.

### 1.4 `expect_completed` (`tidepool-repl/src/session.rs:54`)

A function whose entire reason to exist is "under `ParkedThread` a turn never
returns `Suspended` in-band". It `unreachable!()`s on the variant that becomes
the normal case. Deleted.

### 1.5 `ParkedThread` (`tidepool-runtime/src/session/persistent.rs`)

The `SuspensionMechanism` trait exists to abstract over exactly two impls. Its
`ParkedThread::resume` is an `unreachable!()`. With `ParkedThread` gone the
trait has one impl and abstracts nothing: **the trait, both impls, and
`PersistentSession`'s `S: SuspensionMechanism` type parameter all go**, and the
`PhantomData` with them. `PersistentSession` calls
`machine.run_*_suspendable*` directly.

### 1.6 What does NOT depend on the parked thread

Checked explicitly, because a dependency here would have been the blocker:

- **Nothing inspects or mutates session state while suspended.** The
  `tidepool://session/bindings` resource reads a `BindingsSlot` snapshot
  republished *after* each turn — a plain `Arc<Mutex<Value>>`, never the live
  worker. Under threadless the suspended `Session` is sitting in its slot and
  can be `peek`ed directly (strictly better).
- **`session_reset` while suspended** drops the pending ask. Today that works
  by dropping `response_tx` so the parked `recv()` errors. Under threadless it
  works by dropping the `Session` — the stowed continuation goes with it. Same
  observable behavior, fewer moving parts.
- **Timeout/`Wedged`** uses `PauseGate::request_abort` + the JIT `CancelHandle`.
  Both are thread-agnostic: the gate fires at the next effect dispatch, the
  cancel flag at the next GC/tail-call safepoint. Neither needs the thread to
  be *the same* thread. The engine's `GateDispatcher` already does exactly this
  wrapping with no ask interception.
- **`it` / rebinding / decl-vs-stmt classification / `:commands`** are all
  compile-and-bookkeeping, entirely above the suspend boundary.

---

## 2. The threadless entry points, and the repl's coverage of them

The harness drives suspension through four machine entries:

| entry | materialization | harness user | repl user |
|---|---|---|---|
| `run_fragment_suspendable` | `Value` | `ResidentSession::run` | `run_plain_eval`, `run_reference_fragment` |
| `run_fragment_suspendable_binding` | `Bind{forced}` | `ResidentSession::run_bind` | `run_bind` |
| `resume_suspended` | `Value` | `ResidentSession::resume` | (new) |
| `resume_suspended_binding` | `Bind{forced}` | `ResidentSession::resume_bind` | (new) |

The repl needs two more materializations the suspendable family does not yet
have:

| repl path | non-suspendable primitive | suspendable sibling |
|---|---|---|
| `run_multi_bind` (`(a,b) <- e`) | `run_fragment_and_bind_projected` | **missing** |
| `run_bare_expr` (`it` + render) | `run_fragment_and_bind_render` | **missing** |

`run_bare_expr` is the default path for every bare expression, and
`tests/ask_resume.rs` drives `ask` through `.eval(...)` — a bare expression. So
the `Render` sibling is on the critical path.

**This is cheap, and it deletes code.** `jit_machine.rs` already factors all
four policies into `ResultMaterialization` + `Materialized` behind one
`fn materialize(&mut self, ctx, done_ptr, policy)`. The suspendable path does
NOT use it: `finish_suspendable` carries a `bind_forced: Option<bool>` and
hand-rolls a near-verbatim copy of `materialize`'s `Bind` arm. Replacing that
parameter with `ResultMaterialization` and calling `materialize` gives all four
policies at once and removes the duplicated epilogue.

---

## 3. The mid-stack suspension, and how it becomes data

This is the real work. `run_block` is a Rust loop; item *k* suspends and items
*k+1…n* must still run after the answer arrives. Today the parked thread holds
that loop state on its native stack.

Threadless requires two levels of stowing:

**Per-item tail.** Each run path does work *after* the machine returns. That
tail must survive the suspension as data — exactly what the harness already
does for one case (`resume_bind` threads `binder`/`gen` across the suspension).
The repl needs the same for five paths:

- `run_plain_eval` — the turn's own `DataConTable` + probed inner type.
- `run_bind` — `name`, binder metadata, target generation, defining expr.
- `run_multi_bind` — the binder vector + generation.
- `run_reference_fragment` — the forced type display.
- `run_bare_expr` — the `it` binder + inner type, then `value_outcome_bound_it`.

**Block cursor.** `results` so far, the per-item classify verdicts, the next
index, the suspended item's index/kind, and the `last_value`/`last_type`/
`last_truncated`/`last_value_pos` accumulators. All plain data. Decl batches
never suspend (no machine run), so a suspension can only originate inside a
single item — the cursor needs one pending-item slot, not a general stack.

The cursor lives on the `Session` (which now outlives the turn), so
`Session::run_block` returns either a completed block or a suspension, and
`Session::resume_block(answer)` re-enters at the stowed tail and continues the
loop. `input` (the payload lane) is already cloned per item specifically so it
stays in scope "after an in-block `ask`/resume" — that contract is preserved
by construction, since the cursor carries it.

---

## 4. Target shape

```
tidepool-codegen/jit_machine.rs
    finish_suspendable(…, ResultMaterialization)   ← was Option<bool>
    run_fragment_suspendable_projected / _render
    resume_suspended_projected / _render
    (deletes the duplicated bind epilogue)

tidepool-runtime/session/persistent.rs
    PersistentSession              ← was PersistentSession<S>
    (deletes SuspensionMechanism, ParkedThread, Threadless, PhantomData)
    suspendable bind / project / render + their resume siblings

tidepool-repl/session.rs
    run_block  → BlockCursor state machine
    resume_block(answer)
    PendingTail (5 variants)
    (deletes expect_completed)

tidepool-repl/{worker,state,server}.rs
    SessionManager  → Idle | Running | Suspended{session, cont_id}
    turn runs on spawn_blocking; gate wrapper, no ask interception
    (deletes ask.rs entirely, the worker thread, and both channels)
```

One engine. The repl becomes a single-node client of it.

---

## 5. Risk register

- **GC/tenure on a resumed materialization.** The `Render` policy's
  field1-before-field0-tenure ordering is load-bearing against aliasing; it
  must hold identically on the resume path. Mitigated by routing both paths
  through the same `materialize`, rather than a second hand-rolled copy.
- **A runaway pure loop no longer strands a dedicated thread — it strands a
  blocking-pool thread**, and the `Session` moved into that closure is
  unrecoverable. `Wedged` must therefore drop the whole manager entry rather
  than try to restore a machine it does not hold. `session_reset` already
  replaces the entry wholesale, so the get-unstuck button keeps working.
- **Behavioral oracle is the repl suite** (`scripts/battery-shard.sh
  tidepool-repl`). Decl/stmt/meta classification, `it`/rebinding, `:commands`,
  ask/suspend/resume, `session_reset`, render stubbing must be byte-identical.
