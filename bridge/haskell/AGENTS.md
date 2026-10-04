# Contributor workflow

For this directory's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Configure the pinned native toolchains with
`bash scripts/buck2-configure.sh --tests` in a provisioned checkout, then use
`just build` and `just test-native` for the owning targets. See
[the server build guide](../../docs/swarm-builds.md) for resource admission.
After translation or serialization changes, run `just fixtures-check`;
prepared products are declared Buck outputs, not files to update by hand.
