# Contributor workflow

For this directory's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Use the repository Nix/toolchain environment to verify changed Haskell
consumers. After translation or serialization changes, run
`just fixtures-check`; never edit prepared artifacts by hand. Do not share
`dist-newstyle` between worktrees.
