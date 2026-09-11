# Resident sleep effect

Status: PRD for the next dogfood wave; not implemented by this planning task.

## Problem and MVP

An agent waiting for work can repeatedly wake, inspect unchanged state and spend
inference tokens deciding to wait again. The primary use case is much simpler:
the LLM asks Haskell to sleep for fifteen minutes, and gets a tool result when
that wait finishes. No intermediate inference, polling turns or reminder messages.

Make this an ordinary composable effect. Its continuation stays suspended while
the timer runs; after expiry, Haskell resumes and the enclosing block eventually
returns its final value. The calling agent waits, not the entire swarm.

Proposed user-facing syntax (names must be reconciled with existing duration and
effect vocabulary before implementation):

```haskell
sleep (Minutes 15)
```

Composition uses the same primitive:

```haskell
sleep (Minutes 15)
Cmd.status buildJob
```

One tool call, one eventual result. No user-managed timer handle, background
process, callback actor or rearming protocol is needed for this path.

## Required behavior

- Sleep returns `()` after the requested interval, measured with a monotonic
  clock. Scheduler delay can make it later; it must not complete early.
- Fifteen minutes is a supported ordinary request. Shorter tool-observation or
  foreground-command limits must not convert it into periodic model turns,
  discard its continuation or report a false failure.
- Waiting releases runtime execution capacity. It does not run a shell `sleep`,
  occupy a command-memory reservation or busy-loop a Haskell machine. Other
  actors and unrelated requests continue working.
- The timer's completion resumes the continuation once. Intermediate Haskell
  effects, if any, run without inference; only the final tool result returns to
  the LLM. The runtime must not also send a redundant wake message.
- Normal operator interruption and steering remain usable. An explicit
  cancellation of the sleeping evaluation cancels its timer and continuation;
  its suffix must not execute later. Do not make the operator wait fifteen
  minutes to regain control. Preserve the existing native steering semantics
  rather than inventing a second input path.
- Actor retirement and evaluation cancellation release the timer through the
  existing lifetime owner. Cancellation racing expiry must not resume twice or
  execute a cancelled suffix.
- Use one effect in resident LLM blocks and Haskell actor handlers. A sleeping
  handler retains that actor's sequential handler semantics; it does not freeze
  siblings or enable concurrent mutation of its own state.
- Zero duration completes immediately. Negative or unrepresentable durations
  are rejected before scheduling, without overflow or silent clamping.
- This is live-session suspension, not restart-durable scheduling. Host death
  does not promise that a timer or suspended program will be reconstructed.

## Scope and ownership

Haskell owns the small typed surface; Rust owns timer scheduling and cancellation.
Extend the existing effect interpreter, suspension and actor lifetime mechanisms.
Do not add a parallel scheduler, timer registry or special LLM wake service where
an existing owner can supply the behavior. Inspect the full native tool-wait path:
an asynchronous Rust timer alone is insufficient if an outer layer times out and
forces the model to poll.

The implementation should include the exact signature, imports and one-line
example in the appropriate shared API guidance. Teach agents to choose a useful
wait once rather than narrating unchanged state every thirty seconds. Sleep is
for deliberate delay; existing typed completion sources remain preferable when
the actual event is available. Native Codex goals remain disabled in Shoal.

## Acceptance

1. Through an actual Codex TUI with a scripted/mock provider, execute the exact
   sleep example followed by an observable suffix. Verify no intermediate
   provider requests or periodic tool returns, then one completion and one suffix.
2. Exercise the fifteen-minute duration with controlled time at the timer owner;
   use short real waits for ordinary integration tests. Verify the outer transport
   can sustain the long wait too: accelerated timer tests alone cannot prove this.
   One manual fifteen-minute smoke run with a mock provider is appropriate before
   the first dogfood launch; no paid inference is needed for acceptance.
3. While one evaluation sleeps, another actor makes progress. Operator steering
   can interrupt the wait using the normal TUI path; the cancelled suffix stays
   unexecuted and the TUI remains usable.
4. Cover cancellation/expiry races, actor retirement, zero and invalid durations,
   and a Haskell-handler caller. Use focused tests at their owning boundaries.

## Later uses, not MVP requirements

The same effect can support a Haskell loop that reads command output, tests a
condition, sleeps ten seconds and repeats without inference. A robust stream
consumer must account for command termination, retention gaps and markers split
across pages. That is a separate composition example, not a prerequisite for
shipping sleep. No output-watching DSL, recurring schedule, deadline modifier or
subscription redesign belongs in this MVP.

## Next-wave allocation

Proposed wave: this bounded capability plus remaining
[Codex-use reconciliation and acceptance](../interactive-applications/README.md).
Keep the engine megatask paused for that allocation. Confirm the actual remaining
Codex work against preserved checkpoints; this PRD does not claim it is complete
or change product acceptance. The launch commission should explicitly select this
allocation rather than inheriting the previous two-megatask fan-out.
