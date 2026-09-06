# Service local implementation frontier

Base: dd93b31aa749215c7ad87fa0ad7769dbedc4b0ed.

Custody-before-bootstrap is independent of controller readiness. The custody
implementer owns `fork_workspace.rs`, admission/bootstrap changes, worktree
binding diagnostics and their focused tests. It may prepare the required
`actor_host.rs` integration in its candidate; service TL is the sole merger and
will not edit that file concurrently until the custody candidate is folded.
Reuse ActorRef/Incarnation, BindingTable and existing ownership release paths.
No generic readiness registry. Concrete custody-ready admission contract must
be committed before further custody implementation forks.

Service TL owns backend control design while custody proceeds. The native wire
contract is externally pending, not guessed or published. Backend client/bridge
implementation forks follow a committed consumer-backed interface. Proposed
notification Haskell surface goes to root for agreement first. No native Codex
editing or worker; no running-host replacement.

Independent reviews receive exact candidate commits and implementer handles,
may request contract-local repairs, and retain review obligation through repair.
All reports use inherited WaveDelivery/WaveCheck evidence fields.
