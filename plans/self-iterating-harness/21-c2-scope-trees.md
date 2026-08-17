# C2 scope trees, retirement, and snapshots — design

**Status:** scaffold (2026-08-17). Companion to [PRD 21](21-recursive-companion-prd.md)
lane C2; consumes [the C1 mount seam](21-c1-mount-seam.md)'s "What C2 should
generalize". Decisions and mechanism only.

This doc settles PRD 21 **open question 1** — "the smallest snapshot/overlay
representation with stable identity and correct GC roots" — and the seam
note's two open choices (retirement mechanism, mount volume).

---

## 1. The scope tree

One tree, two planes hanging off it. Neither plane invents its own nesting.

```rust
pub struct ScopeId(pub u64);            // ScopeId::ROOT == ScopeId(0)
pub struct ScopeTree { parent: HashMap<ScopeId, ScopeId>, next: u64 }
```

`ScopeId` is a **monotone counter, never reused**. That is the whole of
"stable identity": a retired scope's id is never re-minted, so a stale
reference is detectably dead rather than silently aliased onto a live scope.
A scope's parent is fixed at mint time and never rewritten — the tree is
persistent in the same sense locked decision 4 means it (everything is
immutable; "write" is "create a descendant scope").

`ScopeId::ROOT` is the flat session. Every existing caller — decl-plane
helpers, `fork`/`forkAll`, the repl's `:bindings`, the C1 mount spike — is a
ROOT-scope caller and stays one. **Every scope-taking API has a no-arg
sibling that means ROOT, and the no-arg sibling keeps its exact present
behavior.** That is the back-compat contract, and it is a proof obligation
(§5), not an aspiration.

### 1.1 Value plane — frames over the existing `BindingTable`

`BindingTable`'s generation machinery is the closest existing primitive to a
scope frontier, so C2 extends it rather than growing a parallel structure:

- `current: HashMap<BindingName, SessionVarId>` becomes
  `HashMap<ScopeId, HashMap<BindingName, SessionVarId>>` — one shadowing
  frame per scope.
- `live: HashMap<SessionVarId, BindingEntry>` keeps its shape (ids are
  globally unique); `BindingEntry` gains `scope: ScopeId`.
- `resolve_in(scope, name)` walks `scope → parent → … → ROOT` and takes the
  first frame that has the name. `resolve(name) == resolve_in(ROOT, name)`.
- `bind_in(scope, entry)` writes only `scope`'s frame; `bind(entry) ==
  bind_in(ROOT, entry)`. The newest-gen-per-name comparison in `bind` is
  unchanged and now applies **within a frame**.
- `iter_current()` stays the ROOT frame (so `binding_names()`, `:bindings`,
  `current_val_modules()`, the rotation-loss enumeration are all unchanged).
  `iter_current_in(scope)` is the *visible environment*: the upward walk with
  child frames shadowing parent ones.

Locked decision 4 falls out of the representation rather than being enforced
by a check:

| Decision 4 clause | Why it holds |
|---|---|
| children read parent declarations | `resolve_in` walks upward |
| children write locally | `bind_in` touches one frame |
| siblings shadow freely, never collide | frames are disjoint maps |
| the parent never gains child declarations by name | nothing ever walks downward |
| escaped closures stay alive by reachability | §2's sole-ownership rule |

### 1.2 Decl plane — `DeclLog`'s generation chain, generalized from a list to a tree

`SessionLib` is already persistent and already immutable: an append-only
`DeclLog` where `Lib.G<g>` re-exports `Lib.G<g-1>` and `hiding`s the heads it
redefines. That chain **is** a lexical environment — it is merely linear. The
minimal change is to make the link explicit instead of positional:

- `DeclTurn` gains `parent: Option<Generation>`. `Generation` stays globally
  monotone (`turns.len()`), so module names never collide across branches;
  only the *link* becomes a tree edge.
- `render_module` emits `module Lib.G<g> (module Lib.G<parent>, …)` and
  `import Lib.G<parent> hiding (…)` — today's rendering with `g-1` replaced
  by `parent_of(g)`.
- `cumulative_exports_before(log, g)` walks the parent chain instead of
  slicing `turns[..g]`.
- `SessionLib` holds `tips: HashMap<ScopeId, Generation>`; `current_module()`
  / `import_line()` / `define_scoped()` / `retract()` operate on the ROOT tip
  and gain `_in(scope)` siblings.

A sibling pair is then free by construction: two sibling scopes each define
`helper`, minting gens 5 and 6 **both with parent 4**. `Lib.G5` and `Lib.G6`
each `hiding (helper)` from `Lib.G4` and export their own; neither is
reachable from the other, and ROOT's tip stays 4 — the parent gains neither.
Compiling in a scope imports that scope's tip module, so "parent declarations
callable in every child" is the re-export chain doing what it already does.

**Flat degeneracy is the back-compat proof:** at ROOT every define has
`parent == g-1`, every fold walks the same turns in the same order, and the
rendered module is byte-identical to today's.

### 1.3 Cross-plane rule is unchanged

A name lives in at most one plane. `materialize_binder` still `retract`s the
decl-plane name before binding — now **scoped**: it retracts in the binding's
own scope, so a child binding `helper` does not retract the parent's
`helper`. The repl's `bind_pure` → `remove_current` direction is ROOT-only
and untouched.

---

## 2. Retirement — `remove_live` + a real deregistration, not sentinel rebinding

The seam note named two candidates. **Chosen: a genuine
`BindingTable::remove_live` paired with a new public
`JitEffectMachine::deregister_persistent_root`.**

Evidence, from the substrate rather than from taste:

- `JitEffectMachine::release_handle` removes only the handle-registry entry
  and **deliberately does not deregister the underlying persistent root**
  ("ownership transferred, not dropped"). Both production callers
  (`materialize_binder`, `mount_handle`) transfer into the value plane, and
  nothing downstream ever gives that ownership back.
- `OldSpace::slots` is push-only; `cursor`/`used` are monotone; there is no
  major or compacting pass anywhere in the tree.

So the sentinel-rebinding convention would leave every retired mount's root
registered and traced for the machine's whole life. It would satisfy the
letter of "the name no longer resolves" while making the accounting claim
false, which is exactly the anti-pattern this lane is told not to commit.

The deregistration path already exists and is already exercised:
`MachineState::deregister_persistent_root` is idempotent, and `close_realm`
is its one caller today (for parked frames' finalized roots and for handle
entries). C2 adds the missing public wrapper and a second, value-plane
caller.

**The public wrapper is a scope-retirement primitive, not a general tool.**
It is named and documented as such (`retire_scope_root`, not
`deregister_persistent_root`) so it does not become a footgun for a future
caller who merely wants a root gone. Its stated invariant, carried at the
definition site: called only on a root the retiring scope solely owns,
exactly once per root, and witnessed by a `persistent_roots_count()`
decrement — a caller that cannot state which scope owns the root is not a
legitimate caller.

### 2.1 Why removal is memory-safe here

- `perform_gc` rebuilds its root vector per collection via
  `extend_persistent_roots`. Nothing holds an index into `persistent_roots`
  across collections, so `Vec::remove`'s shifting is harmless — no
  tombstones, no slot reuse, no index-stability concern.
- Deregistration does **not** touch `OldSpace::slots`. The `Box` cell stays
  allocated for the machine's life, so `RootSlot::addr()` remains a valid,
  dereferenceable address forever — a fragment that already `iconst`ed that
  address still `load`s successfully.
- **Sole-ownership rule (the load-bearing one):** a slot is deregistered only
  when no *other* live `BindingEntry`, in any scope, holds the same address,
  and only after any `ValueHandle` over it has been released. Debug-asserted
  at the retirement site. This is what makes the escaped-closure acceptance
  true: a closure that escapes a child is mounted into a **parent-scope**
  binding whose entry owns its own slot, so retiring the child scope
  retires the child's entries and leaves the escapee's root registered.
  Its captured child-heap objects stay traced transitively through that
  parent root — reachability, exactly as decision 4 words it.
- Retirement retires **by scope, never by shadowing**: a shadowed older gen
  in the same scope stays `live` (fragments compiled against it still
  resolve), matching `bind`'s existing retention rule.

### 2.2 The honest bound

Deregistration removes a root from the **GC trace list**; it does not reclaim
OldSpace bytes. `OldSpace` never frees an arena or a slot cell before machine
drop, and the major/compacting pass its module doc promises is not
implemented. What retirement actually reclaims is the nursery-resident
subgraph hanging off the retired root that the remembered set does not pin.

The practical consequence, stated plainly: **a long-resident session's
OldSpace grows monotonically with the total number of mounts ever made,
bounded per turn, and is reclaimed only at machine drop.** Retirement caps
what stays *traced* (and therefore what a collection must walk), not what
stays *allocated*.

This is bounded and safe to defer: the tenured residue is at most the retired
scope's own bindings, and the whole arena dies at machine drop or rotation —
the same lifetime bound every value-plane binding already has. Reclaiming
tenured bytes per scope requires the major pass, which is out of this lane.

The same bound is recorded in two other places so it cannot be re-derived by
someone hunting a leak: `tidepool-codegen/CLAUDE.md`'s root-accounting
section (alongside the four counted classes) and PRD 21's deferred list.

**Do not** pair retirement with `forget_remembered_range`. That would dangle
a live indirection cell; it is sound only at arena teardown, where the memory
itself dies.

---

## 3. Root accounting — four counted classes, none folded together

C1 established three. C2 adds the one that actually witnesses release:

| # | Class | Read | Retirement effect |
|---|---|---|---|
| 1 | parked continuations | `stowed_roots_count() == parked_count()` | untouched |
| 2 | handle registry | `value_handle_count()` | untouched (a mount already transferred out) |
| 3 | value-plane bindings | `binding_names()` (ROOT) + `scope_binding_count(scope)` | scope's frame goes to 0 |
| 4 | **GC root ledger** | `persistent_roots_count()` | drops by exactly the sole-owner slots retired |

Class 4 is new *as an asserted class*, not as a mechanism — the counter
exists; nothing has ever pinned it. Without it, class 3 can return to
baseline while every root stays traced, which is precisely the false receipt
§2 rejects.

Invariant, asserted at every mutation: retiring a scope decreases
`persistent_roots_count()` by exactly the number of sole-owner slots among
its entries, and leaves classes 1 and 2 unchanged. "Retiring a scope returns
its accounting class to baseline" means classes 3 and 4 both return to their
pre-scope values.

---

## 4. Snapshots — frozen prefix, digest identity, honest receipts

### 4.1 The boundary already exists

`Harness::register_fork_child` computes `checkpoint = parent_transcript.len()`
and seeds the child with the parent's transcript *and* framing, so the
child's assembled request prefix is already byte-identical to the parent's
through the checkpoint. `engine::assemble_request` is
`[system(framing)] ++ transcript`, verbatim and unreordered. The frozen
post-coalgebra snapshot is therefore not a new concept to invent — it is that
prefix, named, digested, and made explicit.

- `SnapshotDigest` — blake3 over domain `b"tidepool-context-snapshot-v1"`
  with length-framed fields (the framing in `tidepool_runtime::cache` is
  private; reimplement the same three lines rather than inventing a second
  scheme), covering `[framing] ++ messages[..checkpoint]` role and content.
- `ContextSnapshot { digest, framing, messages: Arc<[Message]>, frozen_at_turn }`,
  interned in `Harness.snapshots`. `Arc` because the durable model is already
  reference-shaped (`Event::TurnForked` records a position, not a copy) while
  the live model clones per child; sharing the frozen prefix aligns them and
  makes the digest a once-per-snapshot cost.
- `Harness::freeze_snapshot(node) -> SnapshotDigest` — the explicit harness
  operation locked decision 2 asks for. Idempotent: an unchanged transcript
  freezes to the same digest.
- `Harness::fork_from_snapshot(digest, brief) -> NodeId` — mints a child
  whose transcript is the frozen prefix plus the rendered brief. Byte
  stability is asserted by re-digesting the child's own assembled prefix and
  comparing, not by inspection.

### 4.2 Immutability and the new cache root

A `ContextSnapshot` is never mutated. Compaction of a node that has a frozen
snapshot **mints a new snapshot** — a new digest, a new cache root — and
leaves the existing entry, and therefore every existing child, untouched.
This is locked decision 2 verbatim, and it needs saying because today's
`replace_transcript_with_summary` is destructive: it replaces the transcript
with a single message and zeroes the meters. C2 does not make compaction
non-destructive; it makes compaction *not reach* a frozen prefix.

### 4.3 Receipts, and the gap that is real

Recorded per branch invocation: the parent snapshot digest, the shared-prefix
and branch-suffix sizes, and the provider-reported input tokens for the
branch's first turn.

**The cache-metric gap is real and is recorded as a gap, not faked.** Harness
`Usage` carries `input_tokens`/`output_tokens` only; no provider impl parses
`cached_tokens`/`cache_read_input_tokens`; `TurnRequest` has no metadata slot
and nothing emits `cache_control` breakpoints. Therefore:

- `Usage` widens with `cached_input_tokens: Option<u64>` (serde-defaulted, so
  existing `log.jsonl` still deserializes; `Copy + Eq + Default` preserved).
  A provider populates it **only** when the response actually reports it;
  otherwise `None`. `None` means "not reported" and never renders as 0.
- Shared-prefix and branch-suffix are recorded in **bytes** — exact and
  locally verifiable — alongside the provider's own `input_tokens`. There is
  no local tokenizer, so a token-level split of the prefix is not claimed.
- Emitting `cache_control` breakpoints (and thus *causing* provider cache
  hits rather than merely observing them) needs `TurnRequest` widened and the
  Responses-API system/input partition addressed. Out of this lane; the
  prefix stability C2 establishes is its precondition.

Two prefix-stability hazards this lane must not silently inherit: the outer
loop recomposes its **system message** per iteration (iteration count,
operator input, rotation losses), so message 0 is not stable across loops;
and compaction rewrites the framing carried into the next render. Both are
noted here because they cap how much a future cache-breakpoint lane can
claim — neither blocks digest identity, which covers framing explicitly.

---

## 5. Back-compat proof obligations

Not "we believe it still works" — these are the checks:

1. Every no-arg / ROOT-scope API returns exactly what it returns today:
   `binding_names`, `iter_current`, `resolve`, `live_modules`,
   `seed_external_env`, `current_val_modules`, `current_module`,
   `import_line`, `decl_sources`, `current_decl_heads`.
2. The rendered `Lib.G<g>` module is byte-identical at ROOT before and after
   the parent-link change (the existing `render.rs` unit tests plus
   `tidepool-repl/tests/decl_plane.rs`, `shadow_rebind.rs`,
   `tidepool-runtime/tests/session_decl_accum.rs` are the gate).
3. `companion_mount_spike.rs` passes unmodified — it is the flat-session
   mount user.
4. The repl's ten `iter_current` callers and `bind_pure`'s `remove_current`
   are untouched in behavior.

## 6. Fixtures

Per the seam note, in `examples/harness/scope-spike/` (mount-spike is left
alone): a record with **several** function-bearing fields (the transitive
`isClosureType` walk makes it plausible; it has not been exercised), and a
**lens-shaped** mount (function-typed at top level, expected to already
work). Multiple mounts in one window, and mounts in sibling scopes.

The throwaway-bind-then-swap iface minting stays the accepted workaround. It
costs a GHC compile per mount; scope trees do not make a direct `Val.G<g>`
iface-minting primitive unavoidable, so that extract-facing primitive stays
deferred.
