# The two completion epilogues, and what it takes to make them one

Scope: `tidepool-codegen/src/jit_machine.rs`. Feeds section 2 of
`plans/unpark/feasibility-map.md` — the repl cutover needs `Project` and
`Render` on the SUSPENDABLE path, and the cheapest way to get them is to stop
hand-rolling the suspendable epilogue.

## 1. How they differ TODAY

| | `materialize` (non-suspendable) | `finish_suspendable`'s `Done` arm |
|---|---|---|
| entry | `with_active_run`, every plain route | `run_suspendable_shared` / `resume_applied` |
| context param | `ctx: &mut ActiveContext` | `machine: &mut CompiledEffectMachine` |
| policy param | `ResultMaterialization` (4 variants) | `bind_forced: Option<bool>` (2 cases) |
| returns | `Materialized` (slot / value / slots / (slot, value)) | `ParkedRaw::Completed { value, bound_root }` |
| coverage | Value, Bind, Project, Render | Value (`None`), Bind (`Some(forced)`) |

Only the CONTEXT and the PRODUCTS actually differ; the operations are the same
code, copied. Detail, per case:

- **Value.** Byte-identical in both: `heap_to_value_forcing(done_ptr, vmctx)`
  under signal protection, then `surface_error`. Nothing else.
- **Bind.** `materialize` does `take_runtime_error` → null check → optional
  `deep_force` (+ post-force error check) → capture `gc_active_range` →
  `tenure` → return the slot, and stops there.
  `finish_suspendable` does the same MINUS the leading `take_runtime_error`,
  and then continues with two steps `materialize` has no reason to do: stash
  the slot on `self.last_bound_root` (slot park target only), and bridge
  `slot.current()` — the TENURED, rooted pointer, not `done_ptr` — into the
  `Value` the turn's `Completed` carries.
- **Project / Render.** No suspendable counterpart at all.

## 2. What has to hold for them to become one

1. **The context param has to narrow.** `materialize` only ever reaches
   `ctx.vmctx_mut()`; every operation in it is a `*mut VMContext` operation.
   Taking `&mut VMContext` directly is the parameter BOTH callers can supply
   (`ctx.vmctx_mut()` on the plain path, `machine.vmctx_mut()` on the
   suspendable one), and it removes a borrow conflict rather than creating
   one — `ctx`/`machine` are locals/params disjoint from `&mut self`.
2. **The leading `take_runtime_error` has to be harmless on the suspendable
   path.** It is, and it is not even a semantic change: the bridge that ends
   today's suspendable bind calls `surface_error`, which converts a pending
   first cause into `JitError::Yield(YieldError::from(err))` — the exact value
   the hoisted check produces (`From<RuntimeError> for JitError` is the same
   conversion). Hoisting it only skips a tenure that was going to be discarded.
   In practice it is not even reachable: an effectful drive surfaces a runtime
   error through the loop's `Yield::Error` arm before ever reaching `Done`.
3. **The post-materialize products stay on the suspendable side.** `materialize`
   returns the raw products (slot / slots / rendered value); the suspendable
   caller keeps the two things that are its own concern — the machine-level
   stash, and the bridge of the tenured value into the `Completed` payload —
   in the SAME order as today (stash, then bridge).
4. **Reclaim stays armed after the epilogue.** Both suspendable callers already
   arm last (`run_suspendable_shared` and `resume_applied` both arm after
   `finish_suspendable` returns); the reason is unchanged and now stronger,
   since `materialize` is the single place any policy touches `self.session`
   via `tenure` while `arm_reclaim` stores a `*mut self.session`.
5. **`Project` needs a completion `Value`, and it has none.** The non-
   suspendable `Project` produces slots only. `SuspendableOutcome::Completed`
   must carry a `Value`, and its variants are frozen (the harness matches on
   them). Resolution: `materialize`'s `Project` arm also hands back the result
   tuple's real `DataConId`, and the suspendable side completes with
   `Value::Con(con_id, vec![])` — the real constructor, fields deliberately
   ELIDED. Bridging the tenured fields instead would import the bridge's
   depth/size failure modes into a path that today cannot fail after a
   successful tenure, i.e. it could turn a good multi-bind into an error the
   non-suspendable sibling never produces. The products are the slots, read via
   `take_last_bound_roots`.

## 3. Load-bearing orderings, after the fold

- **Reclaim armed LAST** — unchanged, and now structural for both families:
  the only `self.session` touch in any policy is inside `materialize`.
- **Render bridges field 1 BEFORE tenuring field 0** — this becomes structural
  rather than by-hand. When `toWire` is the identity the two fields are the
  SAME heap object, and the field-1 bridge is a complete owned deep copy that
  is immune to the forwarding `tenure` installs. There is now exactly one copy
  of that sequence, reached by both the suspendable and the non-suspendable
  render route, so a suspension cannot desynchronize it.
