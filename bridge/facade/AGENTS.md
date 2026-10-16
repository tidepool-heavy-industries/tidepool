# Generic Tidepool facade

Read [CLAUDE.md](CLAUDE.md) for ownership. Host, provider, operator and actor
workflow changes belong to `exomonad/host`, not this crate.

Compile the generic library and both utility binaries independently. Standard
library emission and materialization use `tidepool-toolchain::source_bundle`;
this package's build and test inputs must not include Harness, actor sources,
workspace scaffolding or browser assets. Keep stdlib capture validation and
compile-report evidence tests when changing their owning modules.
