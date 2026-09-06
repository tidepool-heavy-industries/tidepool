# Runtime and resident machine sessions

This crate owns the high-level compile/run facade, resident machine sessions,
session checkout, turn supervision, and runtime/session failure classification.

- `SessionRegistry` owns checkout admission and settlement;
  `PersistentSession` owns the resident execution contract. Never remove a
  machine, run it, and manually put it back.
- Checkout epochs fence stale timeout, panic, or cancellation settlement. Every
  path settles exactly once.
- `session::workbench` owns frontend-neutral source classification,
  meta-command tokenization, turn templates, and ordered cursors. Frontends own
  presentation and command policy; do not duplicate the classifier.
- Toolchain discovery, fingerprints, diagnostics, and compiled-artifact cache
  policy belong to `tidepool-toolchain`, even when compatibility modules are
  re-exported here.
- JIT root/continuation ownership remains in `tidepool-codegen`; runtime code
  should compose those mechanisms rather than create parallel registries.

## Workbench execution and recovery

- Ordered execution preserves committed prefixes. A later rejection does not
  roll back earlier external effects; a failed effectful unit need not install
  its outer binding. Preserve effect receipts and report uncertainty rather
  than treating a missing binding as evidence that nothing happened.
- Exact transport retries return retained receipts. A newly submitted source
  block is new intent; do not silently replay an uncertain mutation.
- `session::view` and `session::recovery` govern compile-view/recovery boundaries.
  Recreating a runtime does not recreate arbitrary live values or permissions.
- Keep workbench observation formatting separate from execution and authority.
  A compact display may omit decisive evidence; retain the full result and
  expose expansion without rerunning its effects.
- Use focused Nix-backed checks, for example
  `just test-lib tidepool-runtime 'test(<name>)'` or
  `just test-target tidepool-runtime session 'test(<name>)'`. Test rejected
  suffixes, uncertain effects, cancellation, and stale settlement where changed.
