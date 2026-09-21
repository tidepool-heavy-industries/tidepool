# Tidepool architecture

Tidepool compiles typed Haskell programs into native state machines and runs them from Rust.

The compiler worker asks GHC to parse and typecheck source, then uses GHC's normal internal desugaring, optimization, CorePrep, and Core-to-STG stages. Tidepool takes ownership at prepared STG. `Tidepool.PreparedStg` and the execution projection encode a versioned prepared program; no Tidepool Core IR or Core CBOR artifact exists.

`tidepool-repr` owns the prepared execution schema, constructor metadata, identifiers, and durable formats. `tidepool-codegen` validates and lowers prepared programs to Cranelift, owns their heap and collector integration, and exposes the prepared machine. `tidepool-runtime` owns compilation policy, resident sessions, prepared turns, binding generations, and inspection. Metadata-only requests stop at a checked GHC environment and do not create executable projections.

Rust owns processes, providers, scheduling, persistence, resources, and authority. Haskell remains the authored language and model-facing surface. Effect membership expresses intent; Rust handlers enforce runtime authority.
