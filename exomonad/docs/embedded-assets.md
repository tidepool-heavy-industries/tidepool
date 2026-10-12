# Embedded browser assets

The frozen Buck runtime bundle owns the operator UI and the native Harness
runtime from one exact source revision. Nix supplies the pinned Node toolchain
and dependency cache; Buck compiles the Rust contract producer, generates the
TypeScript types and validators, builds the browser, and seals its assets.

Build through `just exomonad-build`, then freeze and qualify the exact bundle
through [the package guide](../../build/package/README.md). The bundle entrypoint
sets `EXOMONAD_EMBEDDED_ASSET_ROOT` to its retained `share/exomonad/web` directory.
The Exomonad toolchain shell does not select browser assets.

The native host verifies the bundle's declared source identity, its actual
compiled Harness schema, and every asset hash before opening the listener.
An explicit absolute `asset_root` must pass the same checks.
