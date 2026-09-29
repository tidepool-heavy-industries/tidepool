# Runtime completion restart checkpoint (2026-09-29)

Checkout: `/srv/swarm/checkouts/tidepool-completion-runtime`, branch `completion/runtime`.
The accepted prerequisite is already integrated in the root tree as
`3d73e2290` and `90b17c8af`. This branch's reviewable runtime sequence is:

1. `fe32541ee4a51923f4419c9c0503614214bf2c0b` — execution-owned
   admission record, immutable admitted source, and independent delivered
   checkpoint settlement.
2. `a6ab1dbb38f2a55997e5b297fb69eea0c9a0fe96` — real Haskell
   after-answer failure and captured-binding parent/child regressions; removes
   duplicate installed-source application.
3. `9a330b2223c5ed1f942c0dfad65f6f7c7b1c5610` — revoked checkpoint
   retains exact release scope until the resident session acknowledges cleanup.
4. `9350b19392328db41df8265a853c1870b9db588a` — exact release
   confirmation is idempotent after success, while a wrong scope is rejected.

No push or live run. The root owner has all four OIDs and application order.
The tree after this handoff commit is clean. No runtime `systemd-run` unit was
running at handoff (`systemctl --user list-units 'tidepool-runtime-*'
--state=running` returned none).

## Verified behavior and evidence

- Real Haskell capture/continuation exact tests: 2/2, 376 skipped. Log
  `target/completion-evidence/capture-haskell-final.log`, retained run
  `target/tidepool-test-runs/20260929T163858Z-531765-battery`.
- Release retry tests, including injected checkout failure/reinstallation:
  4/4, 375 skipped. Log `target/completion-evidence/release-retry.log`, retained
  run `target/tidepool-test-runs/20260929T164145Z-538849-battery`.
- Exact idempotent confirmation tests: 2/2, 377 skipped. Log
  `target/completion-evidence/release-confirm.log`, retained run
  `target/tidepool-test-runs/20260929T164454Z-550156-battery`.
- `bash scripts/dev-shell.sh cargo check -p tidepool --lib --jobs 8` passed
  after release repair; log `target/completion-evidence/release-consumer.log`.
  Formatting and `git diff --check` passed for committed runtime work.
- The earlier exact-continuation prerequisite passed six focused tests and
  the facade library compile; root integrated it separately.

All builds/tests above were admitted via `systemd-run --user --quiet --wait
--pipe --collect --unit=UNIQUE --slice=tidepool-completion-build.slice
--working-directory=/srv/swarm/checkouts/tidepool-completion-runtime
/run/current-system/sw/bin/bash -lc 'COMMAND'` with declared Nix wrappers.

## Unverified publication WIP in this checkpoint

This handoff commit deliberately records the following in-progress source; it
is **not an accepted M2 publication implementation** and has not passed a
post-edit compile or test:

- `tidepool/codegen/src/binding_table.rs`: a proposed exact private-ID
  promotion into a target scope. It validates final current source IDs and
  names before changing visibility, leases the transitive dependency closure
  once per target scope, and lets completion order replace names without
  comparing compiler generations. Target retirement releases those leases.
  Review alias/observation dependency and rollback invariants before use.
- `tidepool/runtime/src/session/mod.rs` and `resident.rs`: an initial
  `PublicVisibilitySnapshot { scope, epoch, declaration_tip, bindings }` and
  per-scope epoch updates at several declaration/binding paths. The exact
  sorted `(name, SessionVarId)` bindings plus declaration tip are the stale
  authority. Epoch instrumentation is incomplete: inspect every public
  mutation path, especially automatic observation collection, host mounts,
  bind failure after a partial set, and private scope retirement. Do not
  use epoch alone to decide freshness. Compile this WIP first.
- `exomonad/actor/src/resident_workbench.rs`: runner checkout helper to
  capture that snapshot; it is not yet wired into
  `ActiveWorkbenchExecution` admission and may currently be unused.

The compiler owner (`/root/compiler_reuse`) owns all GHC worker/protocol
changes and has not yet supplied a callable declaration-join request or
receipt. Its validator will certify an exact public/private source candidate
including instance-only writes; it must not decide stale. `ResidentSession`
compares the paired public snapshot under checkout on Accepted **and**
Rejected outcomes, then owns one synchronous declaration+binding publication
decision. Do not recompile private source against intervening public state or
replace the public tip with a private tip. Keep dispatch serialization until
that join and publication path are proven. Native-demand owner is changing
`tidepool/runtime/src/session/prepared.rs` and codegen prepared-program files;
they confirmed `binding_table.rs` was available to this runtime parcel.

Next steps: compile the WIP with the Nix wrapper, fix errors and remove any
test-only surface; finish paired visibility instrumentation and actor
admission record, then integrate the compiler's typed validator at the sole
resident publication boundary. Add regressions for same-name older-generation
winner by completion order, retraction/instance-only joins, stale Accepted
and Rejected candidate retry, cancellation on both sides of publication,
failed staging with no visible change, and original binding/root lifetime.

Release cleanup distinction: normal machine contention already waits via
`SessionRegistry::checkout_queued`; a failed checkout is terminal/lost or an
unavailable session, and the exact cleanup scope remains in the existing fork
registry as unconfirmed custody. An actor's stopped path retries pending
release scopes via the session owner. There is no polling/reinsertion loop or
new cleanup registry. Final session/forest teardown must report unresolved
terminal custody rather than reporting a successful release.
