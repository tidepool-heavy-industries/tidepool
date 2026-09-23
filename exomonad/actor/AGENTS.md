# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Inspect focused tests beside the request, workbench, or lifecycle module. Use
`just test-lib exomonad-actor 'test(<name>)'`; compile changed consumers and
cover relevant failure and cleanup paths as well as successful replies.
