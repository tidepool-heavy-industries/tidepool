# tidepool-bridge-derive — `FromHaskell`/`ToHaskell` derive macros

**Charter.** Belongs: the `#[derive(FromHaskell)]`/`#[derive(ToHaskell)]`
proc-macro implementations mapping Rust enums/structs to Haskell GADT
constructors and records. Does NOT belong: the traits themselves
(`tidepool-bridge`), any concrete bridged type (`tidepool-bridge-effects`).
