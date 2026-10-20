# Joined demand-compiler review checkpoint

Review source: `d91f6a67a68c69e23feb50ea88d360743e35da08`.
Integration branch: `integration/compiler-query-warm-final`.
This is a review checkpoint, not a release or Persona qualification.

## Scope freeze

Finish the already approved typed-reference and native-availability repairs to
clean, independently reviewable checkpoints. Do not start additional refactoring
waves. Lifecycle source-birth changes need the next architectural review before
implementation. Preserve component evidence separately from joined acceptance.

## Consolidated work

- `5bde7952f1`: compiler-primary projection and bounded World residency,
  preserving final worker-affinity behavior and lifecycle checked-source carry.
- `cab136bc16`: completed compiler-product handoff with exact pending predecessor,
  certified Name associations and prior-source-body demand through one worklist.
- `15b198c2b6`: remove a redundant StableName comparison; the existing local
  finalization admission still compares the actual compiler object and packages.
- `d91f6a67a6`: remaining direct Haskell outcome test consumers migrated.
- Earlier lifecycle `1a497cdf8e`, final affinity `94804bab36`, and compiled-C
  edge test `ea1c047` are integrated (the latter as `5dc363d2b6`).
- Historical runner-evidence fixes are present as patch-equivalent `21f67a0a7c`
  and `7bcfb4d00f`; the physical-owner tranche is present through `3fe27` and
  equivalent fixes. Only optional formatting from `22a1df26f6` is absent.

## Joined evidence

Logs: `/srv/swarm/evidence/inanna/compiler-demand-20261020/integration/`.

| Check | Actual evidence | Limit |
| --- | --- | --- |
| Worker, Haskell source-boot binary, toolchain cfg, actor cfg | `15b198c2b6-joined-consumers.log`: build succeeded, 21 local actions | Compilation only |
| SOURCE/predecessor history | `15b198c2b6-source-history.log`: one selected leaf passed, eleven actual admissions, 7.80 s test duration | Compiler/component history, not Persona |
| Finalized Core | `15b198c2b6-finalized-core.log`: one selected leaf passed, 1.41 s | Includes genuine changed-interface/Core/package refusals |
| Compiled-C native input edges | `15b198c2b6-compiled-output-edges.log`: one executed, failed at RetainedCore role assertion | New edge assertions not yet reached |
| Host activation cohort | `d91f6a67a6-host-binding-cohort.log`: three executed, all failed at the same FromJSON canonical mismatch | None reached its intended activation assertion |

The attempted one-case native label was absent on this revision. The subsequent
ordinary libtest command lacked the named cohort's service source resources and
failed before the intended boundary. These are investigative invocation errors,
not additional product regressions. The declared host-binding cohort owns the
correct deployment/source resources and is the relevant reproduction.

## Decisions needed in the next review

1. **Canonical original versus current compiler finalization.** The joined
   consumer still refuses `Tidepool.Aeson.FromJSON` before activation. Selected
   certificate and expected bytes are retained; the earlier actual differing
   bytes are not. Pinned GHC decoding/re-encoding of the expected interface was
   byte-identical. Next discriminating evidence is the actual current interface
   and its compiler/source/dependency provenance, compared with the selected
   original. Do not replace exact validation with interface compatibility.
2. **Native availability must not select an implementation.**
   `issue_completed_native_availability` currently returns its temporary
   validation projection through a general `OriginalCompilerInputs` value.
   Approved repair: private result containing only completed availability,
   exact offers and retained custody. The caller keeps the selection-issued
   projection/decisions and original namespace. Preserve exact C-to-A edges,
   absent-A refusal and later explicit selection; do not reconstruct receipts.
3. **Replacement source birth versus permission to prepare.** Candidate B is
   currently minted inside a source transaction under A, which appends A to B's
   source DAG. Fencing A then correctly blocks B. A quoted installer may genuinely
   depend on A, so stripping that edge globally is invalid. The proposed closed
   source route uses the existing `PreparedSourceEntry` proof (resolved in an
   empty session), creates an affine provisional B under A's permission, then
   installs under B. Review actual RootCustody contributions, parent prerequisites
   and every failure cleanup before approving this order. ExplicitLive retains
   real A dependencies.
4. **Nonstart test reaches the wrong boundary.** Its quoted fixture has no
   production-issued reload report, so it refuses before publication dispatch.
   Do not add a test-only manual policy or fabricate that report. A genuine
   hosted reload fixture exists, but no existing public lifecycle action stops
   precisely the preparation supervisor while leaving A live. Decide whether a
   narrow fault-injection contract is warranted after the real source-birth
   boundary works. Keep this guarantee unqualified meanwhile.

## Receiving owners

- `audit_native_selection`: typed realization/reference follow-up, bounded
  parser repair and actual ensure-World integration controls. Wire/helper
  checks alone do not qualify zero-ref acquisition or miss restoration.
- `native_offer_scope_repair`: clean source-ready `1bd21777d3` implements the
  approved private native-availability result. Exact review and root warm-lane
  compile/execution remain pending; no cold output lane was provisioned.
- `audit_reload_ownership`: clean `9db4062183` with six earlier counted controls,
  test-layout repair and two now-executed failing lifecycle controls. Source-birth
  proposal only; no new implementation wave.
- `audit_warm_lifecycle`: stood down with blocked genuine nonstart port at
  `/srv/swarm/evidence/inanna/nonstart-discharge-20261020/port-plan.md`.

No fresh Persona, frozen-bundle acceptance or comparable end-to-end performance
measurement is established by this checkpoint. The engine map documents the
integrated dataflow and the costs that remain.
