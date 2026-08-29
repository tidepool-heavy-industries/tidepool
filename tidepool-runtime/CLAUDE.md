# tidepool-runtime — compile/run API and resident sessions

This crate owns the high-level Haskell compile/run facade, resident sessions,
session checkout, turn supervision, and classification of runtime/session
failures.

Toolchain discovery, validation, fingerprinting, diagnostics, timing, and the
compiled-artifact cache belong to `tidepool-toolchain`. The corresponding
modules re-exported here are compatibility surfaces, not policy boundaries.

Session state has one mutable owner at a time. `SessionRegistry` controls
checkout admission; `PersistentSession` owns the resident execution contract;
frontends choose whether admission waits or fails immediately. JIT resource
ownership remains below this layer in `tidepool-codegen`.
