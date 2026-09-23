# Bridge

This directory holds intact components whose Tidepool and Exomonad ownership
cannot yet be separated without redesigning working interfaces. It is a
transitional source root, not a package and not a new shared abstraction.
Crates retain their `tidepool-*` package names while they live under `bridge/`
during this transition.

The `facade` crate currently combines Tidepool's public execution facade with
the Exomonad CLI and actor-host composition. Its expected split is a Tidepool
facade and an Exomonad composition root. `haskell` combines the Tidepool
language/runtime library and extractor with Exomonad actor contracts; those
source trees should eventually follow their product owners.

`protocol`, `mcp`, and `handlers` carry effect schemas, generated Haskell/Rust
bindings, and concrete interpreters that currently cross the product boundary.
Their eventual homes should follow each effect's runtime responsibility rather
than moving the group wholesale. `atomic-write`, `testing`, and `test-data` are
shared support packages retained here until their production consumers make a
single owner clear.

The `tidepool/bridge`, `tidepool/bridge-derive`, and
`tidepool/bridge-effects` crates are different: they implement Haskell/Rust
value conversion and remain Tidepool-owned.
