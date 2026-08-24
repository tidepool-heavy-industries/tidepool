# Continuation parking and resume — frozen contract for downstream consumers

Written for the typed-subagent wave (PRD 18), whose `Agent` effect consumes this
machinery, so it reads this API from a written contract instead of reverse-engineering
`jit_machine.rs` internals. Sequenced to spawn after the realm landing.

STATUS: FROZEN, with one deliberate ADDITIVE amendment (below). Invariants and
the internal/public split settled at step 2; the signature table (§4) filled at
fold, once step 3 had settled the parked entries' signature. This is the
contract that ships.

## Amendment 2026-08-12 (one-session plan, Phase 0) — additive

The one-session consumer (`plans/one-session.md`) extended the API. Everything
below in §1-§4 remains true; these are additions:

- **`ParkKind` covers all four materialization policies** — `Project {
  n_fields }` and `Render { field0_forced }` joined `Plain`/`Binding`.
  `ParkedOutcome` gained the matching INLINE completions
  (`CompletedProject { roots }`, `CompletedRender { root, rendered }`);
  `Completed { value, bound_root }` is unchanged for `Plain`/`Binding`.
- **`ValueHandle`** — an opaque, `Send` id over a machine-side persistent
  root: mint via `handle_from_finalized(id)` (the frame stays parked; the
  payload mints exactly one handle), observe via `observe_handle(h)` (the ONE
  serialization seam — a closure payload observes as `CLOSURE_SENTINEL`),
  deliver via `ResumeInput::Handle(h)` (the payload pointer feeds the resumed
  continuation VERBATIM, no materialization — this is how a closure crosses
  between sibling frames on one heap). Handles are SCOPE-OWNED BORROWS: not
  consumed by observe or deliver; released by `close_realm` of the owning
  realm; a released/unknown handle is a clean typed error.
- **`close_realm(realm) -> (frames_dropped, handles_released)`** — scope
  exit: the realm's parked frames (stowed roots deregistered, untaken
  finalized payload roots released), its handles, and its cancel-flag entry
  go together; sibling realms untouched; the §1 rooting receipt holds before
  and after; idempotent (an unknown/empty realm is `(0, 0)`).

Gates: `tidepool-codegen/tests/realm_handles.rs` (H1 closure-delivery under
GC, H2 scope-exit isolation, H3 Project/Render inline completions), alongside
the pre-existing realm suites.

---

## 1. What a consumer may depend on

Everything in this section is the contract. If it changes, that is a breaking
change and this file changes with it.

**Types.** `ContinuationId`, `RealmId`, `ParkKind`, `ParkedOutcome`.
`ContinuationId` and `RealmId` are opaque newtypes over `u64` — a consumer may
store, compare, hash and order them, and must not synthesise one. Ids are minted
by the machine and **never reused**: a resumed id is permanently spent, and a
re-suspension during a resume mints a FRESH id in the same realm. So an id is a
safe map key with no ABA hazard, and "is this id still live" is answerable by
lookup rather than by bookkeeping.

**Parking.** A parked continuation is registered as a GC root for its whole
parked lifetime — from park until resume, not merely while some child runs. The
receipt for that is an equality a consumer can assert in its own tests:
`stowed_roots_count() == parked_count()` at every quiescent point on the parked
path. If it ever drops below, a parked continuation is protected by nothing.

**Resuming by identity.** A parked continuation is resumed by its
`ContinuationId` alone. The frame replays its own `suspend_tag`, `ParkKind`,
`DataConTable` and cancel flag — a consumer supplies the id and the answer, and
**cannot** supply a table, because resuming a frame against a foreign effect row
is unrepresentable by construction rather than merely discouraged.

**Enumeration.** `parked_count()`, `parked_ids()` (ascending — the ordering is
imposed by the accessor, a `HashMap` has none, so it is deterministic for
callers and tests), `parked_realm(id)`.

**Ordering.** Parked continuations resume in ANY order. The registry imposes
none. This is the capability the whole design exists for, and it is the property
the falsifier's F4 case pins (eight parks, a forced collection between each,
resumed `[3,0,7,5,1,6,2,4]`).

**Error discipline.** An unknown or already-resumed id is a clean typed error,
never a panic and never a silent alias of some later park. A bottom-bearing
answer is rejected WITHOUT consuming the frame — the frame stays parked and
still rooted, so the consumer can retry with a corrected answer (A5).

## 2. Invariants a consumer MUST hold

These are not advice. Two of them are the conditions the GO verdict was
conditional on, and violating either reintroduces a silent-corruption class that
the landing exists to close.

**(a) Prefix compatibility.** Non-empty handled prefixes on one machine must be
EXACTLY EQUAL — same length, same names, same positions. An EMPTY handled prefix
is compatible with anything (that is the outer driver: threshold 0, nothing
handled, everything interposed, so it dispatches nothing and cannot misroute).
The machine enforces this at ENTRY to the parked path — before the turn is
driven, so a refusal means nothing ran — and refuses with
`JitError::IncompatibleHandledPrefix`.

Why equality rather than agreement-up-to-the-shorter-length, since the weaker
rule looks sufficient and is not: with all non-empty prefixes equal, every tag
that is ever DISPATCHED is below the common prefix length, and all realms agree
on what sits at those positions — so no dispatched tag can reach a position
realms disagree about. Accepting a strict EXTENSION breaks that. A declared
prefix is metadata; it does not tell the machine how long the concrete `H`
actually is, because a realm may simply have chosen a lower suspend threshold
than `H` has handlers. An extending realm's tag sits below ITS OWN threshold, so
it is dispatched rather than suspended, and if `H` has a handler at that position
the request reaches the WRONG handler. Silently. That is the misroute this whole
check exists to prevent, so extension is refused.

**Residual, stated rather than glossed.** The check enforces agreement AMONG
realms. It CANNOT verify a declared prefix against the opaque, monomorphized `H`
— nothing at runtime can, since `H` is a type parameter, not data. A realm that
declares a prefix its row does not actually have is still the caller's
responsibility.

**Consumer guidance that makes the residual structurally unreachable — derive,
don't declare.** For an internal caller constructing realms from runtime code
(which is what PRD 18's `Agent` effect is), derive the declared prefix from the
SAME value that constructed `H`, rather than restating it at the park site. One
source of truth means the declaration cannot disagree with the handler stack,
because there is only one thing to be wrong. Under that discipline the
lying-realm case is not a responsibility to discharge — it is unconstructible.
The residual then applies only to third-party callers that hand-write a prefix,
which do not exist today.

**(b) The two suspension paths must not mix.** A continuation held in the single
`suspended_continuation` slot is UNREGISTERED — protected only by the temporal
argument that no GC runs on a suspended machine. A machine holding both a
slot-held continuation and a parked one is unsound: resuming the parked one runs
collections while the slot-held one is unrooted. **A consumer that parks must not
use the slot path at all.** Both directions are asserted (the L7 assert on the
run entries, its sibling on the parked resume), so the illegal state panics
rather than corrupting silently — but the consumer's job is to not reach it.

The practical consequence, which is what makes this a CONVERSION rather than an
addition for any caller lifting a "child may not suspend" restriction: you cannot
park only the child. The parent is in the slot, and resuming the child would
unroot it. Both move to the registry together.

**(c) Cycle-scoped lifetime — amended 2026-08-12 (one-session plan, Phase 4).**
The one-session consumer satisfies this invariant's INTENT (bounded machine
memory) by a different mechanism than machine death: answerer realms remain
cycle-scoped exactly as written ("cycle" = one loop; `close_realm` at
retirement), and the shared machine itself is bounded by an ENFORCED
fragment ceiling with ROTATION at a quiescent loop boundary — a fresh
machine under the same session id, durable state through the checkpoint,
losses enumerated (`Event::MachineRotated` + the next render's
legible-loss note), the reconstruction path exercised in CI
(`acceptance_selfharness::machine_rotation_between_cycles_preserves_durable_state`).
"An immortal unified machine is out" therefore stands: the machine is
ceiling-bounded and rotating, not immortal. The original text follows.

A realm must not outlive its cycle. Dropping a
machine reclaims the session heap, the old-space arenas and all three root
registries; it does NOT reclaim the JITModule's executable memory, because
cranelift-jit deliberately leaks a finalized arena on drop and nothing calls
`free_memory()`. That leak is address space rather than resident memory (the
arena is reserved `PROT_NONE` and demand-paged) and it is bounded, but it is
per-machine and monotonic. **An immortal unified machine is out.** A realm
needing to outlive its cycle is a NO-GO trigger — escalate rather than extend the
lifetime.

The one figure with cross-lane agreement is 256 MiB/machine of VSZ, measured
byte-exactly by two independent lanes. **Do not treat any per-machine RSS
constant as a property of the machine** — three lanes measured three different
numbers (~200 KB, ~125 KB + per-function, ~50 KB) and the verdict carries that
disagreement forward unresolved. What all three agree on is the direction
(monotonic, never reclaimed) and the address-space figure.

## 3. What is INTERNAL and free to churn

Do not depend on any of this; it will move without notice.

- `ContinuationFrame` and every one of its fields. It is not `pub`, and its shape
  is where per-realm state accumulates — it grew in step 2 and again in step 3.
- `ParkTarget`, `ParkedRaw`, and the `into_suspendable`/`into_parked`
  projections. These exist to share one body between the slot and registry paths.
- `park_continuation`, `run_suspendable_shared`, `resume_applied`,
  `install_registries_with_cancel_flag` — private plumbing.
- The `continuations` map itself, `next_continuation_id`, `realm_cancel_flags`,
  and the established-prefix record.
- `suspended_continuation`, `stowed_root_cell`, `nested_child_depth`,
  `enter_nested_child`, `run_child_fragment{,_pure}` — the single-slot/nested-child
  path. A consumer that parks does not touch these at all (see invariant (b)),
  and they are retained for the pre-existing callers, not for new ones.

## 4. Signature table

All on `JitEffectMachine` (`tidepool-codegen/src/jit_machine.rs`). `U` is the
user context, `H: DispatchEffect<U>` the handler stack.

**Enter the parked path — run a turn under a realm.**

```rust
pub fn run_suspendable_parked<U, H: DispatchEffect<U>>(
    &mut self, table: &DataConTable, handlers: &mut H, user: &U,
    suspend_tag: u64, realm: RealmId, handled_prefix: &[String],
) -> Result<ParkedOutcome, JitError>

pub fn run_fragment_suspendable_parked<U, H: DispatchEffect<U>>(
    &mut self, func_id: FuncId, table: &DataConTable, handlers: &mut H, user: &U,
    suspend_tag: u64, realm: RealmId, kind: ParkKind, handled_prefix: &[String],
) -> Result<ParkedOutcome, JitError>
```

`handled_prefix` is the realm's handled effect names for tags
`[0, suspend_tag)`, in position order — the caller builds the decls row, so it
has them. It is checked and, if this is the first non-empty prefix, ESTABLISHED
**at entry, before the machine is driven at all**. That placement is the
contract, not an implementation detail: an incompatible realm never executes a
single effect against a foreign handler stack, whether it would go on to suspend
or to complete. (Checking only at suspension would miss exactly the realms whose
effects all got dispatched — the misroute surface.)

**Resume by identity.**

```rust
pub fn resume_parked<U, H: DispatchEffect<U>>(
    &mut self, id: ContinuationId, handlers: &mut H, user: &U, input: ResumeInput,
) -> Result<ParkedOutcome, JitError>
```

No `table`, no `suspend_tag`, no `handled_prefix` — the frame replays all three.
A consumer *cannot* resume a frame against a foreign effect row.

**Enumerate and inspect.**

```rust
pub fn parked_count(&self) -> usize
pub fn parked_ids(&self) -> Vec<ContinuationId>          // ascending
pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId>
pub fn stowed_roots_count(&self) -> usize                // the rooting receipt
pub fn take_parked_finalized_root(&mut self, id: ContinuationId)
    -> Option<crate::old_space::RootSlot>                // frame stays parked + rooted
pub fn realm_cancel_handle(&mut self, realm: RealmId) -> CancelHandle
```

**Outcome.**

```rust
pub enum ParkedOutcome {
    Completed { value: Value, bound_root: Option<RootSlot> },
    Suspended { id: ContinuationId, request: Value, has_finalized_closure: bool },
}
```

`bound_root` is `Some` exactly for `ParkKind::Binding`, returned INLINE rather
than stashed on the machine — a completed park leaves no frame, so there is
nowhere for a per-frame slot to live and no window for a second realm's
completion to overwrite it.

**Refusal.**

```rust
JitError::IncompatibleHandledPrefix {
    established: Vec<String>, incoming: Vec<String>, mismatch: PrefixMismatch,
}
pub enum PrefixMismatch { Length, Position(usize) }
```

`Length` and `Position` are distinguished because a length mismatch has no
meaningful disagreeing index — reporting one would be misleading.

Raised before anything runs or mutates; the machine is left byte-for-byte
unchanged, so a consumer may catch it, correct the row, and retry.

**Cancellation is per REALM, not per machine and not per park** — a realm's
continuation ids change on every re-suspension, so a handle scoped to an id would
not survive its own realm. Cancelling one realm's handle cannot abort a sibling
realm's run on the same machine. A cancelled realm's flag is NOT auto-cleared;
call `CancelHandle::reset` when done retrying (same discipline as the
machine-level handle).
