# Recursive swarm execution

Use [RECURSIVE-WORK.md](../RECURSIVE-WORK.md) for the installed execution workflow.
Each owner scaffolds, admits a ready Luna frontier, integrates checked results,
and chooses its next local batch. All execution nodes use resident Haskell;
there is no separate tool-only Luna protocol to learn or install.

`Project.Swarm` is an uninstalled policy prototype, with isolated pure scenarios
in `tests/SwarmSpec.hs`. It has no live driver or role-specific worker tools and
is not an alternative execution path. Its contracts are not launch instructions.
