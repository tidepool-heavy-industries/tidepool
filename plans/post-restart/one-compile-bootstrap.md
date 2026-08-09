# One-compile bootstrap + the realm-machine direction

Source: Codex deep-dive (Inanna-run, 2026-08-08), anchors spot-checked by
root against files read this session. Two tracks, deliberately separated:
track 1 is confirmed and lands with the extract wave; track 2 is a
legitimate design direction gated on a spike, NOT a prerequisite for
track 1.

## Track 1 — one GHC compile before the first model call (CONFIRMED)

Not speculative: the lazy lifecycle already exists underneath the
harness. `PersistentSession` starts `machine: None`
(tidepool-runtime/src/session/persistent.rs:265) and the REPL bootstraps
its machine from the first REAL compiled expression
(tidepool-repl/src/session.rs:881). The harness alone wraps the same
substrate in an eager program-shaped constructor
(tidepool-runtime/src/session/resident.rs:196), which is what forces
both seed compiles (answerer: harness.rs:392; outer: driver.rs:540).

Recipe (Codex's, endorsed):
1. Unbootstrapped `ResidentSession` constructor around the already-lazy
   `PersistentSession`.
2. First real run compiles the machine entry directly (mirror REPL).
3. Delete both seed compiles.
4. Emit render + loop from ONE extract invocation — `compile_turn` is
   single-target today (`{target}.cbor`, compile.rs:95); multi-target is
   Phase B's multi-binder machinery.
5. Outer machine boots from render; loop lands as the second JIT
   function in the same machine.
6. Answerer session created lazily; its first model-written block boots
   its machine (no pre-model cost — the block is compiled anyway).

End state: exactly ONE GHC compile pre-model (~30s today, then D1/D2
attack that residual). Sequencing: after Phase B (step 4 depends on it)
and after driver-async/registry-unify fold (both boot sites are in
mid-rewrite files). Natural first item of the extract wave; supersedes
D7's cache-interim if it lands first.

Keep the separate source-level capability rows regardless — a common
super-row would let answerer code import forbidden verbs (runLLMTurn),
weakening the compile-time boundary that the row-scoping work built.

## Track 2 — the unified (realm) machine: separate direction, spike-gated

Today one machine holds ONE parked JIT continuation
(`suspended_continuation: Option<*mut u8>`, jit_machine.rs:164);
`run_child` (jit_machine.rs:2112) can run sequential child fragments
against a parked parent on the same heap but deliberately cannot let the
child SUSPEND (`ChildSuspended`) — which is exactly why the answerer
needs its own session/machine today.

The generalization: `continuations: HashMap<ContinuationId,
ContinuationFrame>` where a frame carries pointer, realm id, table,
suspend tag, pending-kind. GC foundation is ready (MachineState already
traces a Vec of stowed root slots, machine_state.rs:100). Things that
would need realm ownership: pending, binding/decl planes, finalized and
bound root slots, cancellation, effect roster + suspend threshold,
persistent-root retirement, compiled-function lifetime. The last two are
the real cost: roots clear machine-wide only, and Cranelift functions
accumulate in the JITModule — an immortal unified machine grows without
bound.

**The plausible shape is CYCLE-SCOPED**: one machine per loop cycle
(outer + that cycle's answerer tree), dropped at the loop boundary after
State serializes — reclamation by drop, no retirement machinery.

Why this direction matters beyond latency (root's addition): it is the
runtime foundation the FULL-FORK decision wants
(harness-one-model-full-fork, 2026-08-01: node session = one state
across transcript/decl/value planes; fork snapshots all three). Parent
and child continuations coexisting in one heap makes within-cycle fork
inheritance natural — no cross-machine Cheney clone for the common case;
the fork_snapshot 400-LOC clone shrinks to the cross-cycle case, maybe
to nothing. run_child's ChildSuspended wall and the convos-map-of-
sessions shape are both artifacts of the one-slot limit. "It'd unify
things really cleanly" is correct — this is the unification.

Per do-it-right-over-hedged-hybrid: gate on a GO/NO-GO spike (the
continuation-registry + realm-ownership list above IS the spike's
checklist; dispatch metadata per fragment — positional tags + suspend
threshold — is dormant today because every self-harness effect is
interposed at threshold zero, and the spike must confirm it stays
tractable when a general node's base-effect row enters). Do NOT couple
to track 1.
