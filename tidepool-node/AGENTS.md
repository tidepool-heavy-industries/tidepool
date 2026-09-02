# Interactive actor deployment substrate

This crate owns backend-neutral mechanics at the boundary between a Tidepool
actor and an external interactive agent process: durable pushed-message
delivery, connection/proxy transport, process mount boundaries, and node
readiness. It does not own actor
identity or lifecycle (`tidepool-actor`), coding-agent protocols
(`tidepool-agent`), Haskell MCP policy (`tidepool-mcp`), or composition-root
choices.

- `DurableInbox` is the workspace's sole append-and-ack delivery queue. Reuse
  it for operator and actor push channels; do not add another cursor log.
- An ack means the final consumer accepted the exact sequence. Never advance a
  cursor before the last hop succeeds.
- Keep payloads typed until a concrete transport boundary renders them.
- A node proxy forwards MCP; it does not authorize or interpret tools.
- `ProcessMountBoundary` presents one real checkout at a stable model-visible
  path. Worker composition makes its linked working tree writable while
  protecting source and sibling working files; shared Git metadata remains
  writable by design. Root composition makes its source checkout writable
  because integration is root work. This is operational working-file
  separation, not a hardened security sandbox.
