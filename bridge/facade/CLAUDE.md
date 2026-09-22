# tidepool — facade crate + composition-root binaries

**Charter.** Belongs: the `cargo install tidepool` utility and Shoal binaries,
the independent compile reporter, and library re-exports of the workspace's
other crates. The retained selfharness and operator-web source trees are
historical reference material and are excluded from the supported workspace
build. Actual effect/session logic lives in the maintained crates this one
wires together (`tidepool-mcp`, `tidepool-handlers`, and `tidepool-runtime`).
