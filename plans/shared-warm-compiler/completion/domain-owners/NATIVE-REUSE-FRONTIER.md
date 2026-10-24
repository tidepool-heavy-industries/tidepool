# Native reuse: current qualification frontier

Code candidate `dc34094d5e`; G2/G3 remain unqualified.
[STATE.md](STATE.md) owns revision-specific results and
[ARGON-REVIEW.md](ARGON-REVIEW.md) the next adversarial review.

## Boundaries already crossed

Actual G2 no longer loses historyCaptured during GHC optimization. The native
producer issues one coupled retained/opaque policy, carried through dependency
fingerprints, module reuse and the plugin. Ordinary executable definitions stay
opaque; DataCon/DFun/ClassOp representation remains. Authored native rules and
specializations are suppressed, imported rules retained. The actual Core,
interface and reuse test passes; runtime semantics remain partly blocked.

A declaration Join's source-less interface intentionally lacks the ordinary GHC
usage list. Its authenticated descriptor carries the implementation closure.
Retention now consumes that closure and actual installed compiler objects,
without reconstructing historical dependencies from an inventory or rehydrating
them. Completed owners retain their allocations. Two focused owner leaves pass.

Borrowed TYPE inputs now travel with the updated stage context as one
WorldPrimaryInputs value. Source admission no longer reads an older context after
enrichment. Persistent lexical/instance authority remains independently issued.
Negative controls must run in toolchain's owning libtest, not runtime tests
which link the production toolchain library.

## A. Thin interface ownership at completion

Class-default histories cross declaration and shadowing, then fail during later
instance compilation with InterfaceUnownedThinHome(main, Val.G1).

Hydrated input owners retain real HMI associations, but WorldCellInputs /
selectedTypeBodies flatten completion inputs to descriptor and bytes.
retainWorldCellOriginal reads that projection back, adding only direct injected
and produced captures. A transitive thin input can reach retain-only completion
without the association needed to establish its actual owner.

This is the current source-grounded hypothesis. Verify the full path before
repairing. Prefer carrying the selected association; do not remove the
zero-fingerprint refusal, match arbitrary HPT entries by name, rehydrate, or
retain an entire stale HscEnv. Keep SOURCE knots and completed-versus-decoded
ownership explicit.

## B. Runtime retained-value installation

At `f8051efccd`, actual G2 and the migrated historical-native case both fail at
`tidepool/runtime/src/session/admission/scoped_inputs.rs:331:36`.
All nine compiler transactions succeeded in the preceding G2 diagnostic run.

An acquired requirement first checks its stage's exact NativeOriginal terminal:
demander, global ordinal, full binding and actual value handle. Without a match,
the fallback permits only a Produced value with a matching retained checked
association. The refusal is
`!value.is_produced() || !owner.matches_retained_checked_value(...)`.
The evidence identifies the guard, not which disjunct failed.

An older admitted value captured by a fresh declaration may need authority from
the declaration's issuance rather than the value's age. NativeCodeGrant::Home
already carries product/current evidence. This is a design lead, not permission
to bypass exact binding or terminal checks. Preserve request/stage ownership,
ordinal, complete binding, checked artifact and runtime handle. SOURCE terminals
cannot substitute for native execution authority.

## Discriminating tests and acceptance

- Actual G2 preserves both observations, crosses a declaration barrier with an
  unrelated binding present, and consumes the retained declaration again.
- Native historical capture keeps original artifact/group identity after
  shadowing and retirement; dead uses perform no executable acquisition.
- Dictionary defaults, recursive dictionaries and explicit specialization
  preserve behavior. Their current later failures are not passes.
- Retain-only completion preserves actual HMI allocation, refuses foreign
  NameCache/unit/missing/unowned inputs, and excludes unrelated owners.
- Borrowed-TYPE exact/omitted/changed controls execute through a visible checked
  host binding without an operand. A counter prevents vacuous acceptance.
  Explicit operands already have their TYPE in the request primary.
- Genuine SOURCE producer/protocol tests remain. Native INLINE declarations do
  not become SOURCE producers; obsolete observers and assumptions are removed.

Next: Argon's review, then a bounded repair tranche and the actual production
history. Component passes do not qualify the bundle, G3 or fresh Persona.
