# Prepared parking: the F4 implementation contract

Refines `prepared-resume-integration.md` for the first F4 slice. It fixes the
shared frame, evidence and invocation interfaces the lead owns; the answer
builder (F5) and the generated forcing/synthetic-site parcels build on it.
`resume-contract-v2.md` decisions 4–6 remain the governing text.

## What this slice delivers

- A prepared turn that requests a typed effect parks in the machine's
  `ResourceLedger` and reports `ResidentOutcome::Suspended` through the same
  `classify_parked` path Core uses. The request is the observed effect payload
  (the `Union`'s payload constructor), as on Core.
- The parked frame carries prepared evidence: the site's evidence owner, the
  site id, the runner program and its admitted resume entry.
- `abort` consumes a prepared frame without entering it. A host-built or
  handle answer is refused before the frame is touched
  (`PreparedRuntimeError::NotYetSupported`) until F5 lands the validator and
  builder; the frame stays parked and rooted.
- Every turn artifact admits `__resume` beside `__prepared` through an explicit
  projection root, and the session refuses to park a suspension whose runner
  has no admitted resume entry.
- Realm closure retires prepared frames exactly as Core frames.

Not in this slice: ordinary handled effects (Print, file reads): a request
without a typed site is refused with the temporaries released. Live payloads
(`receive`/`serve`) park nothing yet. `HandleOrError` refuses every prepared
suspension, since nothing is handled on this route.

## Frame representation

`ContinuationFrame` in `tidepool-codegen/src/resource_ledger.rs`:

```rust
pub(crate) struct ContinuationFrame {
    pub(crate) cell: FrameCell,
    …
    pub(crate) evidence: FrameEvidence,
}
pub(crate) enum FrameCell {
    /// Core: a heap-stable Box cell registered as a stowed root.
    Boxed(Box<*mut u8>),
    /// Prepared: the continuation handle's own OldSpace root slot, moved
    /// from the persistent-root list to the stowed-root list for the park.
    Slot(RootSlot),
}
pub(crate) enum FrameEvidence {
    Core(Arc<DataConTable>),
    Prepared(PreparedFrameEvidence),
}
pub struct PreparedFrameEvidence {
    pub owner: ProgramId,        // installed program whose site table is authoritative
    pub site: u64,
    pub runner: ProgramId,       // program whose `__resume` entry re-enters the continuation
    pub resume_entry: ValueId,
    pub continuation_rep: RuntimeRep,
}
```

`refresh_continuation_tables` touches only `Core` frames. The rooting receipt
(`stowed_roots_count() == parked_continuations`) holds for both machines: a
prepared park deregisters the handle's persistent root and registers the same
slot address as a stowed root; take and realm closure reverse or drop it. The
slot cell stays with `OldSpace` for the machine's life either way. The prepared
collector traces stowed roots through `MachineState::complete_root_snapshot`,
the one root snapshot both engines use.

## Machine API (`PreparedMachine`)

- `park(continuation, realm, principal, effect_policy, live_payload, evidence)
  -> ContinuationId`. The handle must be live under `realm`. It leaves the
  handle ledger; the frame owns the slot.
- `parked(id) -> Option<(RealmId, &PreparedFrameEvidence)>` (peek, no
  consumption), `parked_ids()`, `parked_realm(id)`, `parked_count()`.
- `take_parked(id) -> (PreparedHandle, PreparedFrameEvidence)`: the frame is
  consumed, the slot returns to the persistent-root list and is re-minted as a
  handle under the frame's realm so it can be passed as a `Managed` argument.
- `close_realm` drops the realm's frames (deregistering their stowed roots)
  and reports `(frames, handles_released)`.

No `PreparedHandle` is ever the continuation token; `ContinuationId` is.

## Engine API (`PreparedEngine`, `tidepool-runtime/src/session/prepared.rs`)

- `ProgramFacts` keeps each installed program's `sites` and `types` tables and
  its `__resume` entry (`resume: Option<ValueId>`), found by identity
  `(entry module, "__resume")`.
- A machine-owned site index `sites: BTreeMap<u64, SiteWitness { owner,
  row }>` is extended in the install transaction before any code is compiled.
  A duplicate id is accepted only when the two rows are structurally
  equivalent (delivery, wire and input type graphs compared by constructor and
  family identity and ordered arguments across the two programs' local
  tables, cycles included); the existing owner stays canonical. Otherwise the
  install is refused with `PreparedRuntimeError::SiteConflict` and nothing is
  installed.
- `decode_settled(program, outer) -> PreparedSettlement` is the one settled
  layer decoder; `run_settled` and `resume_parked` both use it.
- `park_suspension(program, realm, principal, policies, request,
  continuation, table) -> PreparedParked { id, request: Value }`: inspects the
  `Union` layer, observes its payload through the machine observe path,
  renders it through `value_to_json` and reads the typed site id from the
  `typedSite` field the protocol places in the request payload. Unknown or
  absent site, an unresolvable witness, or a runner without `__resume`
  releases the request and continuation handles and returns a typed error.
- `resume_parked(id, answer: PreparedHandle) -> PreparedSettlement`: take,
  enter `__resume` on the runner with `[continuation, answer]`, release both
  handles, decode. `abort_parked(id)` takes and releases.

## Session (`ResidentSession`)

`settle_prepared` returns `PreparedRun::Suspended { id, request }` after
parking on the eval thread; `run_prepared` maps it to `ParkedRun::Suspended`
and lets `classify_parked` mint the hole from the turn's `HoleSeed`, exactly as
Core does. `reenter` dispatches on the engine: prepared `Abort` consumes the
frame and reports the Core abort error text; other inputs are refused with the
frame intact. Frame membership for hole reconciliation reads
`PersistentSession::parked_ids`, which answers for either engine.

## Producer

- `Tidepool.Session.preparedResumeTargetName = "__resume"`; the Rust templates
  emit `__resume q x = TidepoolResume.settle (TidepoolResume.resumeLifted q x)`
  beside `__prepared`.
- `ProjectionContext.projectionRoots :: [SymbolIdentity]` seeds reachability
  alongside `projectionEntry`; a missing root is `MissingPreparedTop`. The wire
  program still has one entry.

## Acceptance for this slice

1. `prepared_turn.rs`: `b <- runLLMTurn @Bool "q"` suspends on both engines;
   the request carries a `typedSite` naming a row of the turn's site table;
   an unrelated expression turn runs while the frame is parked; a host answer
   on the prepared route is refused with the hole intact; `abort` retires the
   hole; the handle count returns to its pre-turn value.
2. Codegen: a retained handle parked under a fresh realm survives a forced
   collection, `take_parked` returns a handle that observes to the original
   value, and `close_realm` on a parked realm reports the frame and clears the
   stowed root.
3. `just fixtures-update` after the producer change (extractor sources are
   fingerprinted).
