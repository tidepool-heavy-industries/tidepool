# tidepool — generic facade and utilities

This package re-exports Tidepool compiler, heap, effect, runtime and declaration
APIs. It owns the build-bound Haskell stdlib, the toolchain stamp utility and
`tidepool-compile-report`. Source-bundle mechanics belong to
`tidepool-toolchain::source_bundle`.

Exomonad host composition, provider transport, operator interfaces, run maps,
actor sources, scaffolding and browser assets belong to `exomonad/host`.
Do not add host re-exports or dependencies here. The generic library and utility
builds must remain independently usable without those inputs.
