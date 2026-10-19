# 03 — Reload API and production acceptance

Baseline: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. The API worker owns the schema,
authored helper/presenter and generated projections; the acceptance worker owns
joined fixtures and exact executed evidence. An independent reviewer inspects
the integrated contract at each wave. Behavior depends on children 01/02; adding
an argument without their admission/cleanup enforcement is not completion.

## Current surface and intended extension

[bridge/protocol/src/effects/reload.rs](../../../bridge/protocol/src/effects/reload.rs)
owns `RlOptions`, `RlPolicy`, `RlRefusal`, `RlRefusalCategory` and
`SrReloadOutcome`. Generated Rust/MCP/Haskell consumers must come from this owner.
[Tidepool.Reload](../../../bridge/haskell/lib/Tidepool/Reload.hs) owns `ReloadInput`,
JSON/schema defaults, `reloadSource`, `reloadHandler` and `reloadPresenter`.
`ReloadInput` currently exposes `also_check` and `accept_cache_invalidation`;
`ReloadOptions` carries checks and the independent surface policy.

Add a Boolean **force**, default false, through the ordinary authored/native
reload route. In generated options use the schema owner's corresponding named
field. Force means: after candidate preparation, stop the actual affected
generation-owned non-LLM actors/resources captured at the retirement fence, and
publish only after required confirmed drain. It does not mean kill all children,
discard receipts, accept uncertain cleanup, or accept unsupported tool surfaces.

Keep two orthogonal decisions:

| Force | Surface policy | Resulting authorization |
|---|---|---|
| false | CompatibleOnly | Preserve tool surface; refuse retirement blockers. |
| true | CompatibleOnly | Preserve tool surface; authorize affected-set shutdown. |
| false | AcceptCacheInvalidation | Permit supported surface cache invalidation; still refuse retirement blockers. |
| true | AcceptCacheInvalidation | Both authorizations, with all candidate/projection/cleanup checks intact. |

No preview call or approval receipt is required. A default blockers receipt is
useful information, not a token that force must consume. Force targets the set
the owner observes atomically at the fence, which can differ from an earlier
inspection. Report exact actual actor references/incarnations and resource
identities, with readable labels only for presentation.

`reloadSource checks` remains the ordinary compatible/default-refusal helper;
the lower generated options and hosted input expose explicit force. Update all
production positional constructors and concrete dictionaries. Do not add a new
parallel “unsafe reload” endpoint or retain silently different native/authored
semantics. No new Haskell example enters a prompt/skill until compiled through
its owning surface target.

## Typed results and reporting

Extend the existing result/refusal owner with typed distinctions sufficient to
represent the real lifecycle. Final constructor names are schema-owner choices;
the required meanings are fixed:

- Unchanged, preparation rejected and surface refused retain their current
  non-destructive meaning. Invalid candidate/unchanged reload stops nobody.
- Default blocked retirement identifies the actual generation and blocker set,
  including unsupported/unobservable cleanup as explicit unavailable evidence.
- Confirmed changed publication includes actual stopped/drained outcomes, fresh
  notebook reset, selected source/generation and surface difference.
- Non-publication after partial stop reports the still-selected generation,
  each already stopped resource, remaining/refused/unconfirmed resources and
  whether admission remains fenced. It cannot reuse wording that promises the
  actor state was unchanged.
- Cleanup uncertain differs from publication uncertain. The latter uses the
  existing durable publication/recovery owner and cannot imply retry is safe.
- A retired old handle yields a typed admission refusal at the consumer. Retained
  read-only receipts/source observations remain accessible where already granted.

Preserve action and close/cleanup causes separately through Rust and Haskell
presentation; do not stringify errors and reclassify rendered labels. Existing
`source_effect::reload_disposition` classifies unchanged as Read, published as
Committed, refusal as Rejected and publication uncertainty as Unknown. Update
that owner for partial/uncertain stop outcomes: a reload that stopped X performed
a real external action even if B was not published. Record stop receipts as actual
effects, independently from the generation-publication result. Neither presentation
nor the enclosing continuation's failure may erase those receipts.

Keep `resume_source_answer`'s settled-owner-before-resume rule. Test that a
published reload remains published when the Haskell continuation errors, sleeps,
is cancelled, or asks for forbidden new A authority. Pure already compiled
continuation and truly neutral independently granted effects may still finish.
Simple precompiled receipt display remains supported; new old-source renderer
specialization/callback registration can refuse. Do not claim the whole Haskell
continuation terminates at reload.

## Production histories and migration

Construct all authority through actual source editing/capture, installed endpoints,
actor start/resource registration and runtime publication APIs. Do not create
fake registries, private generation records, SQL-edited state or test-only grant
factories. Use small adjacent Haskell fixtures and regenerate declared Buck input
mappings. The host's presentation guide distinguishes direct typed dispatch,
provider-facing presentation and notebook display; follow the actual receipt/page
owner for each assertion.

| History | Required discriminating observations |
|---|---|
| Invalid candidate and unchanged configuration | Source changes via production Files owner; invalid typecheck stops nobody; unchanged preserves bindings/services and performs no retirement/extra compile. |
| Dependent typed actors/resources, default reload | Typed blockers; source/current generation/notebook preserved; independent LLM actor still serves; no stop effect. |
| Force with no prior preview | Actual fenced set stopped through owner, cleanup confirmed, B selected with fresh notebook; exact stopped identities in receipt; unrelated independent actors untouched. |
| New dependent registration races fence | Barrier-controlled startup/register/transfer either included in actual force set or refused with confirmed/retained cleanup. No resource absent from both paths. |
| Partial stop and cleanup uncertainty | X actually stops; Y refuses/times out/loses owner. B not falsely published; receipt says X stopped and records selected A/fence status. No implicit restart/replay. |
| Stale issued policy and callback escape | Old A handle, `SourceCallback` transfer, queued source delivery, display/form/watch/route and typed actor start attempt new work after barrier: exact consumer refusal; B unchanged. |
| Self-reload | Existing fixture's pure `aClosure`/old typed value remains valid under retained continuation; simple receipt display/neutral read succeeds; fresh A compile/install/register refuses; coordinator does not wait for itself. |
| Historical native selection | Real issued old value/dependency remains alive while draining or in independent child world; old custody is not optional B root; genuine demanded conflict still refuses. |
| Cancel/death/late outcome | Before fence, during drain, after native response/before END, after END/before commit, after store commit/before acknowledgement. Observe actual typed selected/cleanup states and no repeated effects. |
| Recovery | Published B under edited draft C; exact source/prompts/installer recovered, reset notebook contract retained, selected artifact corruption refused, initializer/effects not replayed. Partial/uncertain retirement uses existing reconciliation and does not resurrect stale handles. |

Migrate
`actor_host::coherent_reload_acceptance_tests::coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work`
deliberately: preserve captured A prompts/source and its already compiled
continuation, but replace the late **new** A Haskell call's success assertion with
the typed retirement refusal. Preserve its B-isolated-notebook and
late-A-cannot-replace-selected-B observations. Add source-bearing callback aliases,
not merely the normal fresh-B path.

Existing source-bound controls to retain/adapt:

- `actor_host::coherent_reload_acceptance_tests::authored_sync_reload_publishes_an_opted_in_surface_and_waits_for_settlement`;
- `actor_host::embedded_recovery_tests::production_coherent_reload_recovers_published_configuration_without_recapturing_draft`;
- `actor_host::embedded_recovery_tests::production_coherent_reload_recovery_refuses_corrupt_selected_installer`;
- `resident_actor::source_effect::tests::reload_receipt_retains_owner_settlement_when_continuation_fails` (discover its exact owning module path with the counted runner before execution);
- the counted `//exomonad/actor:actor_notebook_contract_tests` cohort;
- `declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions` and `declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts` in `tidepool-toolchain`.

The last two carry the required HLS rewrite handoff. Their previous focused
before/after evidence establishes a real seed-policy defect; it does not establish
this reload feature or completion of the stronger Persona hosted gate. Recheck
integrated selectors and exact source/bundle, and retain the genuinely conflicting
required-history refusal rather than weakening it to make force/reset succeed.

## Worker/reviewer waves and exact commands

| Wave | Worker output | Reviewer/integration evidence |
|---|---|---|
| R3a — API/schema | Force default and generated option projections, typed blocked/partial/uncertain results, authored JSON/schema/presenter and caller migration | Schema roundtrip/default/error cases; all four force/surface combinations through actual authored/native entry; no hand-edited generated consumers or preview-token requirement. |
| R3b — Owner composition and early joined case | Join R1/R2 with API at exact area revision; one real authored prepare→fence→default/force→drain→publication→continuation history | Independent source review plus executed actor/host path before broad fixture migration. Compiler and cleanup failure controls remain meaningful. |
| R3c — History/property and recovery | Independent operation-history model plus bounded hosted corpus above; update prompt/API docs once compiled | Review exact integrated candidate, full downstream journey and callback escapes. Compile affected test callers and run nonzero exact selections. |
| R3d — Campaign join/qualification | Reviewed area branch folded into campaign, matching frozen bundle and declared source/compiler/resources | Replay representative whole shared-warm/reload workflow on final bundle; source acceptance, test execution, measured performance and deployment reported separately. |

Future checks run through admitted pinned commands after checkout storage/toolchain
setup. Minimum owning compile labels at the baseline include
`//exomonad/actor:exomonad_actor_unit_tests`,
`//exomonad/host:exomonad_host`, `//exomonad/host:exomonad_host_unit_tests`,
`//tidepool/runtime:tidepool_runtime_unit_tests` and affected protocol/generated
and Haskell surface targets. Discover exact updated labels/components through
the admitted Buck metadata path; a library build alone omits downstream cfg(test)
callers. Host tests require the configured browser inputs even if a selector does
not open a browser.

Example existing counted execution commands for a future integrated checkout:

```sh
swarm-build just test-native //exomonad/actor:actor_notebook_contract_tests
swarm-build just test-lib exomonad-host --exact actor_host::coherent_reload_acceptance_tests::coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work --expected-count 1
swarm-build just test-lib exomonad-host --exact actor_host::embedded_recovery_tests::production_coherent_reload_recovers_published_configuration_without_recapturing_draft --expected-count 1
swarm-build just test-lib tidepool-toolchain --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions --expected-count 1
```

New selectors are proposed obligations until implemented/discovered; never report
a zero-match run or executable link as a pass. Run changed schema/serialization
corpus requirements through their existing owners, including `just fixtures-check`
when compiler translation/serialization changes actually require it. Do not run
full `just verify` as a routine spot check.

Delete obsolete public constructor/callers that omit the force/default contract,
stale help claiming retired issued handles may start new work, duplicated
authored/native reload parsing, and unused test escape hatches exposed during
migration. Keep `accept_cache_invalidation` independent and retain constructor/
type version/original dependency proofs needed by admitted worlds. Do not delete
old value/provenance safety merely because a new notebook starts empty.

Each wave retains base/final source OIDs, actual built artifact/bundle hash,
commands, selected/executed counts, status, affected-set/cleanup and END receipts,
store selected root before/after, presentation evidence and log/report paths.
Use barriers rather than sleeps; retain unexpected typed results before asserting
rendered output. Reviewer acceptance names untested boundaries. Root integrates
the complete area delta; this document does not authorize implementation,
deployment or replacement of an active run.
