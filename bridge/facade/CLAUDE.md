# tidepool — facade crate + composition-root binaries

**Charter.** Belongs: the `cargo install tidepool` utility and Exomonad binaries,
the independent compile reporter, and library re-exports of the workspace's
other crates. The facade also embeds the pinned `exomonad/harness` runtime;
`exomonad/web` remains outside the supported workspace build. Effect/session
logic lives in the maintained crates this one wires together (`tidepool-mcp`,
`tidepool-handlers`, and `tidepool-runtime`).

`src/exomonad.rs` owns CLI/project defaults, tmux startup, and environment
forwarding. `src/actor_host.rs` composes actor runtime, worktrees, provider
launch, and the hosted workbench; extend these owners instead of adding
launchers. Provider transport belongs to the pinned Harness runtime, actor
identity and authority to `exomonad-actor`, and compilation/resident-machine
mechanics to `tidepool-runtime`.

Native Codex goals are unavailable to every Exomonad actor. Worker launch resolution fills
omitted effort from host configuration and preserves explicit overrides. The
prompt catalog freezes one shared base and API guide at startup; do not
specialize that prefix by role or append dynamic binding inventories. Fork
exact parent context at the specified boundary, and keep candidate publication,
accepted integration, delivered baseline, and checked recipient incorporation
distinct. Active-update admission, presentation, and incorporation are also
separate; preserve unavailable and unconfirmed outcomes. Use the provider transport owner's opt-in tracing for cache diagnostics.
