# tidepool-runtime — high-level compile/run API + session substrate

**Charter.** Belongs today: `compile_haskell`/`compile_and_run` (thin API
building a `tidepool_toolchain::artifacts::CompileInvocation` and handing it
to that crate's one policy-bearing compile front door), the resident session
substrate (`PersistentSession`, the `SessionRegistry` primitive, turn
supervision), and the `classify`/`classify_session` half of failure
classification (`failclass.rs` — dispatches over this crate's own
`RuntimeError`/`SessionError`, so it can't live below in
`tidepool-toolchain`, which reuses its `classify_compile`). **Known tension,
narrowed by the 2026-08-24 toolchain carveout:** toolchain location,
validation, fingerprinting, and the compiled-artifact cache moved to
`tidepool-toolchain` (see that crate's CLAUDE.md) — `paths`, `toolchain`,
`cache`, `artifacts`, `diag`, and `timing` below are now thin module
re-exports of that crate, kept so every existing `tidepool_runtime::<module>`
call site keeps compiling unchanged. What remains here is two concerns (the
compile/run API, and the session/turn substrate) rather than the prior five —
still not one cohesive concern, but splitting the session substrate out
further is a separate, sequenced structure lane, not done here.
