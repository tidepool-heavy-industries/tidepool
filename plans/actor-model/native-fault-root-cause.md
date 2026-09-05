# Native fault containment: root cause analysis

## Finding

The native-fault hang is independently reproducible on the current code. It
does not require the historical heap bug. Outside an armed JIT jump boundary,
the signal handler deliberately terminates only the OS thread with Linux
`SYS_exit(0)`. This bypasses the Rust thread runtime and invalidates the
completion contract on which resident session settlement depends.

The analysis below describes the pre-repair behavior. The subsequent repair
replaces raw thread exit with restoration of the default signal disposition,
unblocking and re-raising the original signal, and a nonzero process-exit
fallback if signal delivery fails. Protected JIT calls retain their existing
typed error path. This deliberately makes an unprotected fault fatal to the
shared host; it does not add process isolation or reconstruct lost heap state.

Repair validation: `just test tidepool-codegen 'test(signal_safety)'` compiled
all five codegen test binaries and passed all eight selected tests (552 skipped).
The subprocess regression checks scoped and unrelated threads, a five-second
deadline, and termination by the original SIGILL. Child core dumps are disabled.
Existing protected-fault recovery and destructor tests pass. Formatting and
`git diff --check` pass. Full host-death/recovery integration and richer crash
register diagnostics below remain follow-up work; no live host was crashed.

## Exact causal chain

1. `ResidentSession::on_eval_thread_with_stack` leases the machine and borrows
   it into `std::thread::scope` / `spawn_scoped`.
2. The evaluation thread installs process-wide signal handlers. Only selected
   JIT calls arm the thread-local jump buffer.
3. A fatal signal with no armed jump buffer enters `signal_safety::handler`'s
   fallback: write a diagnostic, then `SYS_exit(0)`.
4. Raw OS thread exit skips Rust destructors and the thread completion epilogue.
   The child retains its reference to Rust's completion `Packet`; no result is
   published and the scope's running-thread counter is not decremented.
5. Native joining returns, but Rust's `JoinInner::join` cannot exclusively
   acquire that packet and panics: `threads should not terminate unexpectedly`.
   This is a panic from `join()` itself, not its usual `Err(panic_payload)` result.
6. `std::thread::scope` catches that panic internally, then waits for its
   running-thread counter to reach zero before propagating it. It never does.
7. The enclosing blocking task therefore cannot reach the workbench's outer
   `catch_unwind`, `settle_retire`, or normal settlement. Its awaited task remains
   pending, and the resident machine checkout remains occupied.

Relevant owners:

- `tidepool-codegen/src/signal_safety.rs`: fallback signal policy.
- `tidepool-runtime/src/session/resident.rs`: scoped evaluation thread.
- `tidepool-actor/src/resident_workbench.rs`: checkout and settlement wrapper.
- Rust 1.93 `thread/lifecycle.rs`: completion packet and joining.
- Rust 1.93 `thread/scoped.rs`: wait before propagating the scope panic.

## Reproduction and historical evidence

A temporary Rust probe at `/tmp/tidepool-native-fault-rca/probe.rs` links the
current `tidepool_codegen` rlib. It installs the real handler in a scoped thread
and executes x86-64 `ud2`, with and without `with_signal_protection`. An outer
`catch_unwind` mirrors the important resident control flow.

- `timeout 3s ./probe protected`: exit 0; protected error and outer return seen.
- `timeout 3s ./probe unprotected`: exit 124; fallback diagnostic and identical
  Rust join panic seen; outer catch never returns.

The probe ran in its own process and temporary working directory. No live Shoal
session was interrupted. No broad test suite was needed for this analysis.

The earlier failed rich-response prototype log is
`/tmp/shoal-ux-rich-response-fixed.log`: the same fallback diagnostic and join
panic, followed by manual termination at 668.620 seconds. The retained crash
record in `tidepool/.tidepool/crash.log` says:

```
sig=SIGSEGV addr=0x00007fffb7fff348 jmpbuf=null ts=1788597868 ctx=stepping main function
```

The context string is the last authored execution label, not a captured stack
frame. There is no program counter, native thread identity, backtrace, or core
dump in this evidence. It proves an unprotected SIGSEGV, but cannot identify its
precise instruction. The prototype's remembered nursery-slot problem is a
plausible upstream trigger, not a proven instruction-level diagnosis of this
particular crash. The final barrier fix and passing rich-response regression do
not fix the independently reproduced containment failure.

## Why small-looking recovery patches are insufficient

- Catching `join()` panics does not repair the leaked scope counter.
- Adding an async timeout can bound one caller's wait, but does not release the
  scoped thread, machine lease, locks, or damaged memory safely.
- Removing scoped threads would avoid this particular scope wait but retain
  leaked completion state and arbitrary abandoned Rust resources.
- Widening `siglongjmp` over the entire resident operation skips Rust cleanup
  and violates the signal wrapper's documented destructor constraints.
- Logging a failure is not settlement. The current stderr breadcrumb changes
  visibility, not runtime liveness.

The handlers are process-wide, so this fallback can also affect an unrelated
thread with no active jump boundary. It must not assume every recipient is a
disposable evaluation thread.

## Recommended repair boundary

Delete the raw thread-exit fallback. An unexpected unprotected native fault
must terminate the process through an async-signal-safe fatal path, preferably
preserving the original signal/core-dump semantics. Do not advertise recovery
of an address space that may contain arbitrary memory corruption or abandoned
locks. This is an explicit availability tradeoff: the current shared host would
exit rather than remain partly alive and indefinitely wedged.

If preserving the rest of a campaign across such faults is required, establish
a supervised process boundary around the resident heap owner. That owner can
retain arbitrary Haskell values internally; no JSON serialization of closures
is implied. This is substantial separate work because actors currently share
resident machinery and live-value custody. Determine the actual sharing unit
before choosing an isolation unit; do not assume one process per actor.

The existing supervisor/recovery owner should observe process death and make
unavailable state explicit. Accepted effects and replies must remain accepted;
recovery must not pretend lost in-memory handles can be reconstructed or replay
completed effects to regenerate them.

## Acceptance tests for the repair

1. Subprocess test: an unprotected hardware fault in a scoped evaluation thread
   terminates within a deadline with a fatal status, never exit 0 or timeout.
2. Same test from a non-evaluation thread after handler installation.
3. Protected JIT fault still returns its typed signal error; normal and ordinary
   Rust-panic paths retain their existing settlement behavior.
4. Host/process-exit integration: an outstanding invocation gets an observable
   terminal failure or explicit connection failure, not a healthy-looking wait.
5. Recovery integration: completed-effect prefixes and accepted reply settlement
   survive according to their durable contract; lost heap state is explicit.
6. Crash evidence contains the original signal, fault address, native thread ID,
   and instruction pointer where supported, without allocation or locking in
   the handler. Label text remains diagnostic only.

Audit the existing protected boundary separately: its safety contract prohibits
skipping destructors, while JIT code can call Rust host functions. Broadening
that boundary is not an acceptable substitute for this audit.
