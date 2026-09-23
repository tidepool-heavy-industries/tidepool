# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For focused verification, use `just test-lib exomonad-worktree 'test(<name>)'`.
Exercise retained-checkout, dirty/conflicting-merge, and cleanup failures when
those paths change. This crate is GHC-free and suitable for ordinary Cargo
tests.
