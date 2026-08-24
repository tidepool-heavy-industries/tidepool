# tidepool — facade crate + composition-root binaries

**Charter.** Belongs: the `cargo install tidepool` MCP server binary, the
composition-root `tidepool-selfharness` binary (driver + gate + provider +
memory store + web server), and library re-exports of the workspace's other
crates. Does NOT belong: any actual effect/session/harness logic — that
lives in the crates this one wires together (`tidepool-mcp`,
`tidepool-handlers`, `tidepool-harness`, `tidepool-web`).
