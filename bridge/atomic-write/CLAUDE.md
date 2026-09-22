# tidepool-atomic-write — the one atomic write-then-rename helper

**Charter.** Belongs: same-directory temp-file-then-rename writes in two
durability tiers (`write_durable` with fsync, `write_best_effort` without) —
the one mechanism every durable on-disk store in the workspace (worktree
registry, agent binding table, self-harness checkpoint, run lease, toolchain
stamp, session compile cache) uses instead of hand-rolling its own. Does NOT
belong: hard-link-based exclusive-claim writes (a different primitive, kept
with its one caller), content-addressed caching itself
(`tidepool-toolchain::cache`).
