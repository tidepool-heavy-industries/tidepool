# tidepool-atomic-write — the one atomic file publication helper

**Charter.** Belongs: same-directory atomic replacement in two
durability tiers (`write_durable` with fsync, `write_best_effort` without),
and durable exclusive creation (`write_durable_new`) —
the one mechanism every durable on-disk store in the workspace (worktree
registry, agent binding table, self-harness checkpoint, run lease, toolchain
stamp, session compile cache) uses instead of hand-rolling its own. Does NOT
belong: content-addressed caching itself
(`tidepool-toolchain::cache`).
