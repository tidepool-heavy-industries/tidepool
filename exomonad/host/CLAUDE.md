# exomonad-host — Exomonad runtime composition

This package owns the Exomonad host, provider integration, operator interfaces,
run-map inspection and the `exomonad` and `exomonad-view-helper` binaries.
`src/exomonad.rs` owns CLI/project defaults, tmux startup and environment
forwarding. `src/actor_host.rs` composes the actor runtime, worktrees, provider
attachment and hosted workbench. Provider transport belongs to the pinned
Harness runtime; actor identity and authority belong to `exomonad-actor`;
compilation and machine sessions belong to `tidepool-runtime`.

The host embeds actor Haskell, scaffold defaults and browser source provenance.
The `tidepool` facade supplies its build-owned stdlib. `tidepool-toolchain` owns
source-bundle emission, materialization and validation. Development identity
covers the ordered pair of stdlib and actor roots; source capture must validate
both against the exact build-owned identity.

Workspace capture, inspection and drift exclude the exact `.exomonad/state`
tree used for durable memory. Other authored directories named `state` remain
inputs. Keep accepted source, installed tools and shared writable files distinct.

Native Codex goals are unavailable to every actor. Preserve explicit effort
selection and the shared prompt prefix. Context publication, integration,
delivery and recipient incorporation are separate outcomes; unavailable or
unconfirmed evidence must remain visible. Use the provider owner's tracing
for cache diagnostics.
