# C1 mount seam — mechanism note

**Status:** landed (2026-08-17). Companion to [PRD 21](21-recursive-companion-prd.md)'s
lane C1. Decisions and the seam's shape only — no narration; see
`tidepool-harness/tests/companion_mount_spike.rs` for the acceptance sequence
and git history for how it was found.

## The seam: `ResidentSession::mount_handle`

A finalized closure already crosses producer → consumer by
[`tidepool_codegen::jit_machine::ValueHandle`] (pillar B: tenure at suspend,
mint a handle, deliver via `resume_handle` into a parked continuation). C1
needed the SAME payload to cross into a **name**, resolvable in a window that
has no parked continuation to deliver into yet — a decl-scope installation,
not a continuation resume.

Two new `ResidentSession` methods (`tidepool-runtime/src/session/resident.rs`)
close the gap, both composed entirely from existing primitives:

- `current_binding(name) -> Option<(SessionVarId, SessionModule, ValueTier,
  Option<String>)>` — reads an already-materialized value-plane binding's
  identity (its `Tidepool.Session.Val.G<g>` iface + `SessionVarId`), the same
  data `materialize_binder` writes on an ordinary `x <- e` bind completion.
- `mount_handle(name, id, module, tier, type_display, handle)` — redirects
  that identity to resolve through a `ValueHandle`'s tenured root instead of
  whatever it was bound to before. Mechanically identical to
  `materialize_binder`'s own handle-to-`BindingEntry` step (`handle_slot` +
  `release_handle` + `BindingTable::bind`), generalized to accept a handle
  that arrived by ANY path, not just this turn's own completion.

**The pattern:** mint a real `Val.G<g>` iface/`SessionVarId` cheaply, by
running an ordinary throwaway bind of the mounted type (its OWN tenured value
is thrown away), then swap in the real value's root via `mount_handle`. GHC
never needs to see the real value — only its type, which the throwaway bind
already established correctly. No new GHC-facing mechanism, no disk-iface
code written by hand.

Both methods are session-scoped, not node-scoped: `Val.G<g>` bindings live on
the shared `PersistentSession`, so once producer, the throwaway bind, and
consumer all attach to the same session (`Harness::adopt_session` +
`force_attached`), the mount is visible to every one of them by construction
— no per-node wiring needed beyond what `session_bind_context` already does.

## Two substrate gaps this spike found and fixed

Both are pinned as regressions by the acceptance test now that they're fixed;
neither is specific to "mounting" — they were latent because no prior caller
tried an ordinary `x <- e` bind of a type that MENTIONS a function without
BEING one.

1. **`isClosureType` (haskell/src/Tidepool/GhcPipeline.hs) checked only the
   top-level type**, not whether a function arrow appears anywhere in the
   type's structure. A record with a function FIELD (`data Mounted = Mounted
   { applyMounted :: Int -> Int }`) classified as Tier0 (deep-forced before
   tenuring), and deep-forcing tried to force through the function field and
   crashed. Fixed to walk transitively — through type-application arguments,
   newtype representations, and data-constructor fields — mirroring
   `Tidepool.Translate.typeMentionsEffectMonad`'s walk. A bare function type
   still classifies Tier1 exactly as before; the only new True cases are
   ones that previously crashed.
2. **The value-plane BIND completion's rendered value always bridged
   STRICTLY** (`heap_to_value_forcing`,
   `tidepool-codegen/src/jit_machine.rs`'s `finish_suspendable`), regardless
   of tier — a SEPARATE bridge from the tenure step, and the one `finalize`'s
   closure path had already solved for itself with
   `heap_to_value_forcing_tolerant`. A Tier1 bind's completion value (echoed
   back for `:t`/rendering) could contain a real `TAG_CLOSURE`, which the
   strict bridge rejects. Swapped to the tolerant bridge — a closure field
   renders as the same `CLOSURE_SENTINEL` stub `finalize` already uses; the
   live value is unaffected (it resolves through the tenured root, not this
   rendered copy). Strictly a superset of the old behavior: a Tier0 value
   never reaches a `TAG_CLOSURE`, so this is a no-op there.

## Root ownership: three counted classes, not two

Before this spike, the parking contract's invariant was `stowed_roots_count()
== parked_count()` (continuation registry) plus `value_handle_count()`
(handles minted over a finalize payload, released on delivery or realm
close). A mounted binding is neither — it is a **value-plane root**
(`BindingTable`/`OldSpace`, already the mechanism behind `x <- fork …` and a
living decl-plane helper), counted by `ResidentSession::binding_names().len()`
(`iter_current` — newest gen per name).

The acceptance test pins the transition at every mutation:

- `handle_from_finalized` (producer's finalize): `value_handle_count` 0 → 1.
- `mount_handle`: `value_handle_count` 1 → 0 (transferred out — a mounted
  root is NOT tracked by the handle registry once mounted) and
  `binding_names().len()` 0 → 1 (the mounted-root count).
- Producer's own `terminate_node` (realm scope-exit): `binding_names().len()`
  unchanged — the value plane outlives the realm that produced it, by design
  (the same property that already lets a decl-plane helper survive its
  defining loop's retirement).

A mounted root is deliberately session-lifetime, like every other value-plane
binding — this spike does not claim it returns to zero, only that it is
NEVER silently folded into the parked-continuation count, and that the
handle-registry class it transits through DOES return to zero.

## What C2 should generalize

- **Per-mount lifetime.** Today a mount lives until the whole session
  machine drops (`BindingTable`/`OldSpace` have no per-name eviction). C2's
  scope trees need mounts to retire with their owning scope — either a
  `BindingTable::remove_live` (a genuine new primitive: nothing evicts a
  `live` entry today, only `remove_current` un-shadows one) or a convention
  of one mount per scope generation, retired by rebinding the name to a
  sentinel and letting the scope's own retirement race be irrelevant because
  nothing references the old id anymore.
- **Multiple mounts per window, and mounts of non-function-bearing but still
  Tier1-shaped types** (a lens, a capability token) — the seam is generic
  over `(SessionVarId, SessionModule, ValueTier)`, so nothing here is
  `Mounted`-specific; C2 should confirm the SAME two fixes cover a lens
  (function-shaped at top level, already worked) and a record of several
  mounted fields (not yet exercised — plausible given the transitive walk,
  worth a fixture).
- **The throwaway-bind-then-swap pattern is a workaround, not the final
  shape.** It costs a real GHC compile per mount (to mint the iface) even
  though the mounted VALUE is already live. If mount volume matters in C2's
  daily-dogfood lane, minting a `Val.G<g>` iface directly from a known type
  (without compiling a throwaway expression of it) is the natural next
  primitive — deferred here because the spike's bar is composing existing
  primitives, not adding a new extract-facing one.
