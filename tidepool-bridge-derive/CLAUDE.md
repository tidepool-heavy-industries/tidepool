# tidepool-bridge-derive — `FromCore`/`ToCore` derive macros

**Charter.** Belongs: the `#[derive(FromCore)]`/`#[derive(ToCore)]`
proc-macro implementations mapping Rust enums/structs to Haskell GADT
constructors and records. Does NOT belong: the traits themselves
(`tidepool-bridge`), any concrete bridged type (`tidepool-bridge-effects`).
