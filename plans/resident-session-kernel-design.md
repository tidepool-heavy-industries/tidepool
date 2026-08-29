# Resident-session kernel — retired

The implementation described by this plan has landed. This file remains only
as a temporary link target for the actor-model plan while that plan is being
edited concurrently; it is not active design or standing architecture.

Current contracts live in:

- `tidepool-runtime/src/session/kernel.rs` for policy-free suspension traits
  and checkout admission;
- `tidepool-runtime/src/session/registry.rs` for atomic machine ownership;
- `tidepool-runtime/src/session/resident.rs` for resident execution, root
  custody, and runtime/lexical scope pairing;
- `tidepool-runtime/CLAUDE.md` and the consuming crate charters for ownership;
- `docs/continuation-parking-contract.md` for the JIT parking boundary.

The harness intentionally has no default suspension TTL. Hole age is exposed
through `Aged`; a frontend may choose a reclamation policy. Session-owning
nodes reject concurrent checkout immediately, while attached shared-session
windows use notification-driven admission.

Delete this compatibility stub once the actor-model plan links directly to
the owning APIs above.
