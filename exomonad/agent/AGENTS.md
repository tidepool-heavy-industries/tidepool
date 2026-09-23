# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For command-policy changes, run the focused
`goal_policy_is_preserved_for_fresh_resumed_and_forked_launches` test in
`exomonad-agent` and inspect adjacent command tests. For input-control changes,
run its correlation, failure, and cleanup tests plus the tests in `process.rs`.
Compile changed host consumers as well.
