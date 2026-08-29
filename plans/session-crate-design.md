# Session-crate extraction — retired

This exploratory packaging plan is no longer active. The session substrate
currently belongs to `tidepool-runtime::session`; extracting a new crate is
not a prerequisite for the actor model and should be reconsidered only from
real dependency pressure, not from this historical proposal.

The binding rules are current and live elsewhere:

- one machine-session registry and checkout mechanism;
- one parked-continuation and root ledger;
- frontends own admission and timeout policy, not machine state;
- actor orchestration belongs above the session substrate rather than inside
  the JIT or provider adapters.

See `tidepool-runtime/CLAUDE.md`, the `session` module documentation, and
`plans/actor-model/implementation.md` for current ownership.

Delete this compatibility stub once the actor-model plan no longer links to
this path.
