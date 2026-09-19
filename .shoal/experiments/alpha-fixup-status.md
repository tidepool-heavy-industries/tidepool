# Alpha fixup status — 2026-09-18

All implementation workers retired with StoppedNow; root owns final verification.

- Qualified actor-state default import integrated. Root repaired retained-handler
  notebook regression and ran it at 4 GiB: 1 passed, 374 skipped, clean cleanup.
- Authored .shoal now participates in the ordinary source COW snapshot; runtime
  trees are excluded through shared policy, build storage remains independent.
- Reload roots retain their namespace guard, are refreshed at native activation,
  and released on workspace retirement. These are root repairs to worker WIP.
- Four source-reload tests passed before the final activation/lifetime repairs.
- Native workspace suite compiles: two tests pass, two stop during namespace
  preparation with Operation not permitted, before native isolation assertions.
  Host-level retry is required; this is not a passing isolation test.
- Child native edit -> reload -> grandchild inheritance still needs live proof
  on the rebuilt host. The current host predates these Rust changes.
- Role-based reload tool attenuation is agreed design, NOT implemented. No
  model-name restrictions or automatic nudge defaults have been introduced.
- The bounded Luna self-nudge experiment was blocked before installation by
  the old read-only mount. No hook, token, or self-nudge was created.

Successor: rebuild/restart, run host namespace checks and native child reload
acceptance, then clean-user/package/cache release gates. No release publication,
version tag, or forced remote update has occurred.
