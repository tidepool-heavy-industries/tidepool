# Tidepool

This repository builds two products in one Rust and Nix workspace:

- [Tidepool](tidepool/README.md) compiles and runs typed Haskell effect programs
  as resident Cranelift state machines.
- [Exomonad](exomonad/README.md) builds persistent agent applications, typed
  tools, and coordination on Tidepool.

The transitional [`bridge/`](bridge/README.md) contains shared and mixed
components whose final ownership is not yet resolved.

To create and run an Exomonad workspace, see the
[getting-started guide](exomonad/docs/getting-started.md). For development,
start with [contributor guidance](AGENTS.md) and the repository's `justfile`.

Copyright Inanna Malick. Licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md).
