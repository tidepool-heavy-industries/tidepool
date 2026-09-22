# Tidepool

Tidepool compiles typed Haskell effect programs into Cranelift-backed state
machines driven from Rust. Haskell describes the computation; Rust executes it
and services its effects.

Live evaluation runs notebook cells in a resident machine session. Successful
declarations and values enter the persistent declaration environment and
binding store, so later cells can use them without replaying earlier cells.
The runtime retains compiled continuations, heap values, and the authority
associated with concrete resources.

Installed capabilities use the same compilation and execution machinery but
are compiled before use. A capability can therefore be invoked repeatedly
without paying the per-cell compilation cost. Effect membership expresses what
the Haskell program may request; Rust handlers still enforce concrete runtime
authority.

Reload captures and typechecks the configured Haskell source roots, then
publishes the affected module graph atomically. A successful reload changes the
source revision used by later compilations. Existing bindings keep the code
they were built from. A rejected reload leaves the previous compiled graph
active and preserves the edited files and diagnostics for repair.

The compiler, heap, execution engine, toolchain, runtime, and Haskell/Rust value
conversion crates are under this directory. The extractor and mixed Haskell
library currently remain under [`../bridge/haskell`](../bridge/haskell) until
their Tidepool and Exomonad responsibilities can be separated cleanly.
