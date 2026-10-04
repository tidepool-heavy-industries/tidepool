# Freer retention boundary

`freerRequest` is a real `freer-simple` `send` program. Its returned `E`
constructor contains a function continuation. The retained-result tests inspect
that outer constructor and keep the continuation as an opaque managed handle.
They do not recursively force it or add a second effect interpreter.

`//bridge/haskell:freer_retention_prepared` compiles `FreerRetention.hs` with
the pinned production compiler and target `freerRequest`. The complete generated
directory is a test runtime resource. Tests read `freerRequest.prepared.cbor`;
no prepared artifact is stored in source or embedded in a Rust executable.
