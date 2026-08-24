# tidepool-runtime — high-level compile/run API + session substrate

**Charter.** Belongs today: `compile_haskell`/`compile_and_run`, on-disk path
resolution (`paths.rs`), the compiled-artifact cache, the resident session
substrate (`PersistentSession`, the `SessionRegistry` primitive, turn
supervision), and the toolchain/deploy handshake. **Known tension:** this is
a genuine catch-all bundling five distinct responsibilities (paths, cache,
compile, session, turn supervision) rather than one cohesive concern — that
reflects current reality, not an endorsed target shape; splitting it is a
separate, sequenced structure lane, not done here.
