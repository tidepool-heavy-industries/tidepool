# G2 peer++ review checkpoint

Updated 2026-10-21.

## Status

This is a review-ready but **unqualified** G2 checkpoint. Package linkage is
green. Host lifecycle work has advanced to its real failure frontier, where
five actor controls remain red. No matched bundle or Persona run exists.

| Item | Value |
|---|---|
| Branch | `g2-closeout/deepseek-20261021` |
| Code candidate | `5c759554c6f9478bc2344ef17ee026c6ec23fed0` |
| Immediate base | `e77034bea0`, merged P `7a685c7e99` and N `083ae94f12` |
| Remote | `github/g2-closeout/deepseek-20261021` |
| State | WIP review checkpoint; not G2 acceptance |

This note supersedes the older c910d1 checkpoint as the branch's current source
description. The later sections of [STATE](STATE.md) remain useful history but
describe an earlier source state and review frontier.

## What passes

The joined consumers compile from `e77034bea0` with all later WIP changes
needed for the diagnostics:

```text
swarm-build bash scripts/dev-shell.sh just check \
  tidepool-codegen tidepool-runtime exomonad-actor tidepool-toolchain
```

- Build `d3f5dcd2-c8e6-4f30-a84e-a88f5154feda`
- `1849` local actions
- `BUILD SUCCEEDED`
- Log: `/srv/swarm/evidence/inanna/g2-joined-e77034bea0/joined-check.log`

The registered package cohort passes:

```text
//tidepool/toolchain:toolchain_package_occurrence_linkage_tests
```

- Declared and executed target count: `9`
- Build `3ac09b34-08d6-43c9-82c1-59e5279ae809`
- Buck target: `Pass 1; Fail 0`
- Log: `/srv/swarm/evidence/inanna/g2-joined-e77034bea0/package-cohort.log`

The actor lifecycle cohort reaches the real host-boundary failures:

```text
//exomonad/actor:actor_n2_publication_tests
```

- `1 passed; 5 failed; 24 ignored`
- Build `7f5d36a5-2f4a-4555-b96f-58e7242469fe`
- Log: `/srv/swarm/evidence/inanna/g2-joined-e77034bea0/actor-n2-diagnostic-6.log`
- The passing case is
  `resident_workbench::request_tests::host_only_private_execution_publishes_its_exact_retained_text`.

The diagnostic log was produced with:

```text
RUST_LOG=tidepool::session::private_host_mount=debug,exomonad_actor=debug
```

## Package work now accepted as a component

P's cumulative repair carries one stage manifest through ordinary targets, item
selection and cached HOME products. The important repairs were:

- Cached candidate package sidecars are authenticated and retained in the
  current package-interface custody.
- Cached package-bearing HOME groups receive target-qualified roots rather than
  being treated as fresh-only exceptions.
- Receipt decoding accepts package roots for issued cached HOME groups, not only
  fresh receipt modules.
- Exact inherited HOME rows are normalized to the prior issued product after
  full owner/origin/group/import equality, preserving immutable token custody.
- The repeated-Fresh oracle now checks `Arc::ptr_eq` for the HOME product and
  package bindings in both target orders.

An earlier attempt only relaxed the completion issuance comparison. It still
failed with sidecar custody for `Data/List.hi`; accepting structural equality
without carrying the prior issued token was therefore rejected. The final path
canonicalizes to the prior exact product instead of weakening `same_issuance`.

An earlier producer manifest emitted roots only for `FreshProduct`. That failed
as soon as a valid cached HOME occurrence carried package imports. Including
all cached products unconditionally was also rejected because it could attach
roots for unused implementations. Roots are included only for HOME groups whose
binders are demanded by the target.

## Host lifecycle WIP: what changed and why

The previous N candidate still routed all private host mounts through hidden
binding classification. Once package linkage was repaired, that produced
`visible classification` failures before any publication work.

The current WIP records the exposure mode explicitly:

```text
PrivateHostBinding::Hidden(admission)
PrivateHostBinding::Visible(admission)
```

- Hidden mounts remain absent from the public World and continue to block freeze
  until retirement.
- Visible mounts remain eligible for effect acceptance and later publication.
- Both share the same pending-host set and retirement owner.

Skipping hidden mount registration was tried and rejected because a live hidden
owner must still return `WorldCellPublicationIncomplete`.

Other current hypotheses and their rationale:

- Use the current private compile-view key when planning a later cell. Host
  mount and retirement legitimately advance the key; the execution's original
  key is only the birth baseline.
- Allow a completed checked item to begin a later item in the same private
  execution. Completion describes the preceding cell, not permanent closure.
  Freeze still closes the execution.
- Stop comparing the immutable birth binding-tip ID on every later cell and
  freeze. Accepted private host writes advance the binding tip; exact private
  scope, owner, source and current World association remain checked.
- Validate retirement records as an exact historical chain, then compare their
  owner/source/target/logical World facts with the current association. A later
  private cell may advance the association after a valid retirement.
- Borrow the captured test source in the test-only compilation authority instead
  of admitting an unowned empty source. This repaired the host-only test's
  unrelated fixture refusal and exposed the remaining runtime behavior.
- Add stage-specific debug context around mount, checked-prefix start, freeze and
  retirement validation. Failures were otherwise collapsing into the same
  `StaleStagedDeclaration` or `WorldCellPublicationBaseStale` value.

These are hypotheses under review, not an accepted lifecycle design.

## Current red frontier

### Visible abandonment then publication

`abandoned_visible_host_mount_repairs_private_world_and_allows_publication`

- Mount, abandonment, retirement and later cell execution now work.
- Freeze fails with `WorldCellPublicationBaseStale`.
- Relevant code: private retirement settlement in
  `tidepool/runtime/src/session/paired_publication.rs`.

### Hidden retirement then more work

`hidden_private_input_retires_then_allows_publication`

- Mount, live-hidden incomplete-freeze refusal and owner retirement work.
- The following cell execution is refused with `StaleStagedDeclaration`.
- Relevant code: private checked execution/prefix admission in
  `tidepool/runtime/src/session/admission.rs`.

### Command-job carrier

`successful_private_cell_publishes_declarations_and_accepted_host_writes_only`

- The command-job carrier cannot be prepared because a native root is outside
  its retained view.
- This happens before the lifecycle code can publish the accepted write.

### Retained Text visibility

`tool_result_mounts_retain_original_checked_interfaces`

- The Text mount and acceptance complete, but the next compiled cell reports
  `Variable not in scope: tool_one`.
- Runtime binding visibility and compiled source visibility have diverged.

### Multiple abandonments

`two_abandoned_visible_host_mounts_retire_before_publication`

- Both names do not disappear within the test timeout.
- The diagnostic run records a background retirement refusal with
  `StaleStagedDeclaration`.
- Relevant code: retirement-chain validation in
  `tidepool/runtime/src/session/resident.rs`.

## Architectural question for peer++

The failures no longer point to one missing comparison. Private execution state
is split across:

- immutable birth library, key and binding tip;
- current private compile-view key;
- checked-item progress;
- pending visible/hidden hosts;
- completed host writes;
- retirement records and current World association;
- actor test source authority.

A review should decide which owner advances each transition and which invariants
remain immutable. Avoid continuing with independent equality exceptions at
each stale-check site. The likely boundary is one private-execution lifecycle
that owns current key/tip/progress/readiness and emits one publication
association, while source/custody remain immutable.

## Not done

- No Discovery or notebook joined acceptance at this WIP tip.
- No historical/cold/reopen publication acceptance.
- No matched frozen bundle.
- No fresh DeepSeek Persona run.
- Full `just lint` is not green. It reached repository-wide Clippy and stopped
  on 33 errors outside the five changed files; this is not acceptance evidence.

## Preserved work and cautions

- Preserve dirty H instrumentation and completion-custody WIP at
  `/srv/swarm/checkouts/tidepool-deepseek-g2-h-20261021`.
- Preserve D's three-file patch and diagnostic history named in [STATE](STATE.md).
- Do not edit or switch `/srv/swarm/checkouts/tidepool`.
- The diagnostic logs are large because `RUST_LOG` also enables actor timing
  events. Retain them, but use the relevant line ranges rather than attaching
  them wholesale to review.

## Recommended review order

1. Decide the private-execution ownership boundary above.
2. Review the typed `Hidden`/`Visible` mount distinction as part of that state.
3. Review command-job retained-view preparation separately from publication.
4. Determine why accepted visible host values are absent from the next compiled
   source view.
5. Only after those decisions, repair retirement settlement and rerun the two
   smallest actor controls before the full six-case cohort.
