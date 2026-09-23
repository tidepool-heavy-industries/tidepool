# tidepool — facade crate + composition-root binaries

**Charter.** Belongs: the `cargo install tidepool` utility and Exomonad binaries,
the independent compile reporter, and library re-exports of the workspace's
other crates. The retained `exomonad/harness` and `exomonad/web` source trees
are historical reference material and are excluded from the supported
workspace build. Actual effect/session logic lives in the maintained crates this one
wires together (`tidepool-mcp`, `tidepool-handlers`, and `tidepool-runtime`).

`src/exomonad.rs` owns CLI/project defaults, tmux startup, and environment
forwarding. `src/actor_host.rs` composes actor runtime, worktrees, provider
launch, and the hosted workbench; extend these owners instead of adding
launchers. Backend protocol/process details belong in `exomonad-agent`, actor
identity and authority in `exomonad-actor`, and compilation/resident-machine
mechanics in `tidepool-runtime`.

Keep `InteractiveGoalPolicy::Disabled` uniform at the shared Exomonad launch
entry point, including root and resumed nodes. Worker launch resolution fills
omitted effort from host configuration and preserves explicit overrides. The
prompt catalog freezes one shared base and API guide at startup; do not
specialize that prefix by role or append dynamic binding inventories. Fork
exact parent context at the specified boundary, and keep candidate publication,
accepted integration, delivered baseline, and checked recipient incorporation
distinct. Active-update admission, presentation, and incorporation are also
separate; preserve unavailable and unconfirmed outcomes. Reuse Codex's
`CODEX_ROLLOUT_TRACE_ROOT` for cache diagnostics rather than adding a competing
logger.
