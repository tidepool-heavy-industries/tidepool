# tidepool-runtime

High-level Rust API for compiling Haskell into prepared-STG programs and running them through Cranelift.

- `compile_haskell` returns a checked prepared program and constructor metadata.
- `compile_and_run` and its cancellable variant execute a one-shot effect program.
- `session` owns resident prepared machines, declaration generations, value bindings, scoped imports, and threadless continuation parking.
- `artifacts`, `cache`, and `toolchain` delegate compiler discovery, validation, and prepared artifact caching to `tidepool-toolchain`.
