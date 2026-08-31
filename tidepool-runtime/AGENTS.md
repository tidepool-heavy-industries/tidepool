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
