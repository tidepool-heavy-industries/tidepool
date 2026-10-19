# Reload implementation handoff

Original contract/base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Area: `/srv/swarm/checkouts/tidepool-shared-warm-reload-20261019`, branch
`feature/shared-warm/reload`; supervisor `/root/warm_compiler_campaign/reload`.
Current implementation checkpoint: `d9efb188fb1ee27f06901440eca8ccf20c7a72c4`, joining campaign contract
`923fd554a0` through `aebe74eac5` and reviewed Ready repair
`4e790ab850f4df886fc185e15a7b28139f6bf530` through `3fcbbb4bfc`.
The joined `345194431d` production/test configurations compiled. Its bounded
Ready control passed; full feature acceptance remains withheld. Campaign owns
shared contracts and qualification; this file owns the area continuation.

## Current disposition

The R1 foundation implements explicit private source dependencies, exact generation
admission under the existing runtime lexical-scope lease, original callback
provenance, generation-filtered directory admissions, bootstrap first-publication
fencing and uncertainty retention. Independent LLM worlds have their own issuers;
typed children retain external parent dependencies. Missing provenance refuses.
The mandatory HLS handoff was read: retained bytes, selected roots, exact original
dependencies and permission to compile remain separate. Preserve strict namespace
validation and `RequestNativeAvailability::for_compiler_roles`.

Full R1 remains incomplete. The public runtime source transaction boundary and
fresh deferred compilation consumers require the approved repair below. WatchRoute
provenance, forms, reply-tool definition installs and callable display admissions
remain required, held successors. R2 selective retirement/drain/publication and
R3 hosted migration are not wired. `force=true` explicitly refuses before reload
preparation in `source_effect.rs` (`f147c79413`); surface-invalidation consent is
independent. It must not be silently accepted before selective drain exists.

The reviewed Ready parcel adds admission at the actual legacy prepared-install
consumer using Ready metadata's exact lexical scope. Its control proves positive
installation first, then refusal with unchanged state, then precompiled pure
completion under the same fenced scope. Public facade fixtures now use real
PersistentSession/ResidentSession entry points instead of naked private callback
issuers. The Ready control and legacy public facade control now pass natively. The
certified inherited facade control fails during initial declaration setup before
its producer. The minimal Sleep fixture now imports the real `minutes` helper
through the canonical template builder imports argument (`d9efb188fb`); an earlier
appended import failed the protected insertion-marker contract and remains
retained failure evidence. No assertion was relaxed.

## Executed evidence and independent blockers

Evidence directory:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reload/`.
Exact tested source: `f147c794130203ff9bf21096cbf6f910f38ea62e`.

- The four-target owning production/cfg(test) compilation exited 0 after two
  bounded compile repairs (ResidentSession implementation module privacy and
  BoundBinder fixture field). Both unit executables linked. The actor production
  default output is compile metadata, not a standalone linked rlib. Exact command,
  artifacts/hashes and attempts: `early-native-four-target-pass-receipt.json`,
  `early-native-four-target-build.log`, `early-native-four-target-retry02.log`,
  `early-native-four-target-retry03.log`.
- Runtime counted foundation: 5 selected/executed, 4 passed, 1 Ready-install
  refusal failure. Four source-admission owner/history controls passed. The
  reviewed Ready consumer/test repair above remains unexecuted.
- Actor counted foundation: 11 selected/executed, 7 passed, 4 failed. Bootstrap
  unknown cleanup, publication distinction, typed-child prerequisite, transferred
  callback, actual compiler ticket uncertainty, old alias and typed bootstrap
  owner controls passed. The new joined Sleep fixture failed before its control
  because `minutes` was not imported; the bounded import repair is now committed.
- The existing capture selector failed before capture-END assertions at mixed
  grandchild checked compilation (`start.rs`): NativeGroupUnavailable ordinal 8,
  CertifiedSelection candidate_count 1. Both existing quoted-source generation
  controls failed their initial source-original versus public-context commitment
  assertion before durability. Preserve exact inputs and both identities. These
  are unresolved product/fixture causal questions; qualification/campaign own a
  controlled baseline discriminator. No validator or oracle waiver is authorized.

Counted reports, exact selectors, artifacts and full stderr are retained in
`foundation-runtime-focus/`, `foundation-actor-focus/`, their corresponding logs
and consolidated `foundation-counted-receipt.json`. Real Sleep/bind/facade work
plus injected aggregate Unconfirmed does not establish a real post-resume GHC END
fault. Current public-owner uncertainty/panic controls remain pending.

Subsequent joined native evidence (separate from the original foundation):

- Exact `345194431d9f8407a7b7e7c4c273eafbce90bfc0`: all four runtime/actor
  production and cfg(test) targets compile, exit 0, 26 local actions. Receipt
  `ready-sleep-four-target-receipt.json`; no tests executed by that build.
- Same source: runtime focus selects/executes 3, passes 2, fails 1. Ready's real
  install/refusal/unchanged-state/pure control and legacy public facade pass.
  Certified inherited facade fails at `facade.rs:442`, initial define_scoped_in
  returning StaleStagedDeclaration before facade production. The fixture changed
  ROOT to a newly minted isolated notebook; investigate actual capture/source
  initialization rather than relaxing exact-context/native validation. Failed
  case diagnostics are incomplete, compiler cleanup not_observed, inputs retained.
  Receipt `ready-facade-counted-receipt.json` and `ready-facade-focus/`.
- Exact `d9efb188fb1ee27f06901440eca8ccf20c7a72c4`: corrected Sleep selector
  selects/executes/passes 1, exit 0. Actual resumed Sleep then hidden binding runs
  Generation/rootBootstrap/typedBootstrap variants, with pure/refusal controls.
  The aggregate Unconfirmed is injected; this is not a physical post-resume GHC
  END fault. Process cleanup is confirmed, compiler cleanup not_observed and
  diagnostics_complete false. Receipt `sleep-binding-counted-receipt.json` and
  `sleep-binding-focus02/`. Preserve these limits despite the behavioral pass.

The std-only protocol generator, generated mirror refresh and snapshot check
passed earlier (tests executed: 0); receipt `protocol-generator-receipt.json`.
Generated consumers still require their owning gates. Pinned format/source-map
checks passed the foundation; repeat after each joined source change.

## Approved C6 primary repair: one actual runtime transaction owner

Shared decision: campaign `923fd554a0`, C6 public runtime compiler owner receipt.
Root and campaign approved implementation; escalate a concrete inability to
separate physical phases without replay or to observe actual END, not routine
signature choices. Do not introduce a registry, gate, trait framework, public
None/Bool permission, or callback that can replace authority with Ok(None).

Freeze the intended runtime entry at the existing ResidentSession owner:

```rust
with_source_compiler_transaction<T>(
    source: Arc<RuntimeLexicalScopeLease>,
    workload: CompileWorkload,
    cancellation: CompilerTransactionCancellation,
    supplemental_hook: Option<SourceAdmissionHook>,
    close_sink: impl FnOnce(CompilerTransactionClose),
    action: impl FnOnce(&mut Self, &mut dyn FnMut(CompilerTransactionClose)) -> T,
) -> Result<CompilerTransactionSettled<T>, SourceAdmissionError>
```

The nonoptional existing exact ORIGINAL source lease is canonical authority;
validate its own owner/epoch/identity separately from recipient placement and
check Open lazily at actual fresh source work. Supplemental hooks can restrict
but never supply missing lease authority. The entry reuses existing
`settle_compiler_transaction_cancellable_for_workload`; lower concrete source
claims settle at its SAME real physical outer END/abandon. Make stacked source
setter/claim-transfer helpers private. For off-checkout/static producers the same private runner lends a concrete
RuntimeSourceCompilerTransaction capability, issued only inside that real owner
entry. It binds the original lease and actual origin and cannot escape or remain
usable after END (borrowed lifetime/closed state); no public constructor or test
factory exists. Session wrappers delegate to this same runner. Public
PersistentSession/ResidentSession facade producers require the actual owning
entry or typed refusal. Foreign/raw
nested transactions refuse; borrowed inner NotStarted never settles outer holds.
Pure continuations and empty/unchanged facade paths remain claim-free.

Pre-machine factories keep their actual preparation ticket. Collect its clean
physical END BEFORE final machine/scope adoption and runtime-owned entry.
Unknown/abandoned preparation retains exact obligations and primary failure plus
independent cleanup uncertainty, and refuses advancement without automatic retry.
The existing actor `run_source_transaction` delegates once to the runtime owner
after final adoption; it does not enclose that phase in another ticket.run.

Retain the strong exact final lease in immutable SourceDependency/bootstrap
ownership without an ActorGeneration back-reference cycle. Actual issuer timing:

- Fresh factory: ROOT is placement placeholder before machine creation, never
  an admissible final source lease. Preparation END precedes final adoption.
- Root/program-root: adopt at actual final RootScope/program preparation before
  authored startup/source aliases, rather than waiting for Behavior.start.
- Typed/independent child: adopt after checkpoint remint/fallback/new-machine
  resolution and before mailbox/source transfer aliases. Typed capture retains
  exact parent A; independently captured LLM world has no external prerequisite.
- Recovered root: adopt after actual public-owner epoch transfer invalidates
  old admissions, before startup release/authored resume. Do not refresh stale
  leases or recreate gates from generation integers.
- Transferred callback: retain original source A even when placed in recipient B.
  Source eligibility and recipient custody are different checks.

Once bootstrap C's canonical lease is exposed, all world and runtime admission
consults that same C. First publication fences C before IO without claiming C
itself (external prerequisites remain checked), refuses outstanding source claims,
and retains the fence through visibility/ack. Confirmed success reopens SAME C;
proven no-effect nonpublication may reopen. Unknown remains closed permanently
for old aliases; recovery issues a new context from the selected actual head.
A private observation context cannot authorize source work.

Owned path cone: runtime `session/{admission,source_admission,resident,persistent,
prepared,facade}.rs`; actor `termination.rs`,
`resident_workbench/{generation_admission,replacement}.rs`, actual root/child/
recovery issuers in `resident_workbench.rs`, `start.rs`, `mount.rs`, `local_actor.rs`
and their direct constructor consumers. Coordinate host introspection query
admission with host_inputs; provider-world/capsule ownership is not eligibility.
The bounded analogue review additionally confirmed actual retained checked-cell
declaration adoption through begin_prepared_cell_item, plus public split
stage/render/validate/adopt/commitReceipt fronts. Gate actual fresh side effects
and commits through the SAME owner, including retained artifacts; rendering
without durable identity allocation can stay pure. Exact turn.rs renderer and
prepare/compile_cell_program_admitted producers likewise require actual source
owner capability, not raw CompilerTransactionOrigin. These exact additions are
approved; category search is frozen before the core checkpoint/early compile.
No broad registration/drain scope is authorized in this primary repair.

Include confirmed adjacent deferred compiler owners in the SAME repair:
PendingPreparedInstall/Source currently lacks provenance at actual off-checkout
Cranelift compile; retain exact source capability and refuse before fresh compile
or registry mutation after fence. Its fresh synchronous Cranelift claim lasts
through actual compilation completion, not a fictitious GHC END or an earlier
snapshot checkout END. Separate cache reuse from new compilation and final install. Public host-value materialization compiles before its later install
gate; legacy carrier writes before later admission. Close at actual fresh producer,
not another test-local check. Already installed precompiled execution stays neutral.

Required joined controls: positive public facade producer, unknown physical END,
panic/abandon, retired source; root/typed bootstrap plus generation hidden binding;
pure continuation claim-free; source A transfer into B; fresh deferred compile
refusal before registry mutation; preparation uncertainty prevents second phase;
first-publication same-gate fence; source-neutral independent world continues.
Compile owning production and cfg(test) callers at first coherent checkpoint.

## Owners, admission and exact continuation

Two retained descendants, both `gpt-6.1-sol`, high, fork none; original starts
2026-10-19 07:52 UTC, no descendant capacity:

- `generation_admission`: isolated admission checkout, source terminal
  `4e790ab850f4df886fc185e15a7b28139f6bf530`, now ACTIVE implementing approved
  C6 on joined `345194431d`, with the area Sleep fix `d9efb188fb` supplied. It
  sends a coherent frozen core/direct-caller checkpoint before native admission.
- `authority_review`: report-only `REVIEW-BASE.md` owner, completed independent
  cumulative foundation, bounded Ready and exact `345194431d` C6 analogue review.
  Dormant after terminal category report, reserved for exact new owner candidate
  full-history review, including issuer timing and actual control entry points.

Area collected terminal Ready/Sleep results and released its active native lane.
Qualification/campaign
own baseline discrimination for preserved context/native-original failures.
Host_inputs owns optimized PublicModule query consumer and receives the final
fresh source admission API before consuming it. No source/native command is
running at this handoff update; no orphan command observer exists.

The owned bind mount and pinned --tests tools are realized; no browser gate yet.
Latest refreshed admitted projection was 60.92 GiB with only qualification
baseline actor and reload both budgeted at 14 GiB. Earlier stale-scope rejection
(93.1 GiB) remains retained and superseded by producer-confirmed admission, not
bypassed. All area native jobs are now terminal; idle daemon memory stays in
global Current and does not represent an active reservation.
Latest measured build cgroup peak 13,410,226,176 bytes exceeded the initial 12 GiB
estimate; future compile allowance is 14 GiB, 32 GiB prospective growth. Before
launch refresh build.slice current plus remaining per-scope budgets (subtract
actual currents already included), keep below 80 GiB and disk >=100 GiB+growth.
Campaign's test lane and qualification Persona are terminal; at the latest
producer receipt only qualification baseline actor (14 GiB) remained active.
Host/owned/warm successors were source-only, not admitted native lanes. Refresh
producer receipts and actual scopes before any next launch.
Installed swarm-build storage flags remain unsupported. Use supported wrapper,
local-only repository commands; no daemon restart/cleanup or ambient compiler.

```sh
swarm-build just build -j1 //tidepool/runtime:tidepool_runtime //exomonad/actor:exomonad_actor //tidepool/runtime:tidepool_runtime_unit_tests //exomonad/actor:exomonad_actor_unit_tests
swarm-build just test-lib tidepool-runtime --exact session::resident::authored_publication_tests::source_fence_keeps_precompiled_plain_execution_and_refuses_ready_install --expected-count 1
swarm-build just test-lib exomonad-actor --exact resident_workbench::request_tests::joined_continuation_binding_keeps_all_source_worlds_uncertain_at_end --expected-count 1
```

Host production/unit consumers and codec/Haskell caller gates remain pending;
configure --tests --browser before host. No full qualification claim.

Event-driven continuation: terminal producer includes exact task/base/candidate,
evidence/status and next action to area plus affected dependents. send_message to
running recipient; followup_task to dormant recipient. No periodic polling.
Awaited implementation event: generation_admission exact coherent owner candidate
with prod/cfg(test) evidence or concrete escalation; area reviews critical code,
reactivates authority_review, repairs/folds then runs joined focused controls.
Awaited discriminator event: qualification/campaign exact controlled baseline
results; preserve current failing inputs until that event.
