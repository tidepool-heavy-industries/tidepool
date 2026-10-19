# Campaign handoff

Implementation continues. This file supports backend/account continuity; it is
not a stop order, completion claim or authorization to change live deployments.
Read [STATUS.md](STATUS.md), [RUST-CLIENT-MIGRATION.md](RUST-CLIENT-MIGRATION.md)
and [SERVICE-API.md](SERVICE-API.md) before resuming implementation.

## Exact campaign owner and accepted source

- Owner: `/root/warm_compiler_campaign`, gpt-6.1-sol high.
- Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-campaign-20261019`.
- Branch: `campaign/shared-warm-compiler`; remote `github` is authoritative.
- Last published checkpoint before this handoff: `c634de8fa60d5fcfc862b82b43221b93efef081c`.
  Push session47034 settled exit0. Tested product is unchanged parent
  `aa59c48c030371b1b107b181c46c1dbb6e2d9cfb`.
- Dirty scope while writing this handoff: only campaign plan Markdown. No source
  WIP or native command is owned by campaign. Read actual `git status --short`
  and HEAD; do not reset/stash another owner's work.
- Parent `/root` coordinates the performance fork. Release/main/live-host owner
  is unconfirmed. Campaign publication is authorized, release/deploy is separate.

Private operational handoff directory:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/campaign/`.
`CONTINUATION.json` records awaited producer, recipient and actual next action;
`LIVE.json` records live native grants and observers. These supersede static
resource samples. Never assume an ended turn schedules its own continuation.

## What passed and what did not

At f88, nine production/cfg(test)/actor consumers compiled, workflow154 and counted
Rust27 passed;103 hashes verified. At aa59, actor cfg(test) compiled and actual
service-backed public-module queries executed2/PASS2;19 hashes verified.
Exact receipts: `campaign/foundation-join-f88/TERMINAL.json` and
`campaign/query-qualified-join/TERMINAL.json`. Actual frontend strict answer guard,
its wrong-name negative, history and source-domain/reexport controls passed.
Helper zero-old-work counters do not establish frontend-zero-work; no frontend
HostWorkObservation was emitted. Per-report cleanup/diagnostic/directory hash limits
remain. Build times are not runtime speedups.

Ordinary-cell Prepare, actual per-item runtime consumer, private successor, atomic
bindings/world publication and second-cell use remain unfinished. Exact completed
dependency/HPT/validity/recovery reuse is CORE (arm B), not optional. Arm C's deeper
import/GRE reuse remains conditional. Full force/drain/recovery, strict matched
Persona and actual query/appended-cell measurement are still required. Existing
user/evidence files stay preserved; old authority formats explicitly refuse.

## Actual continuation

1. Warm owns real Rust Prepare/output certification and the actual demanded input
   issuer. OWN rowsad094 supplies cold primary constructor plus decoder derived
   from SAME output/reservation/captured manifest, not a complete hot adapter.
   Warm must explicitly wake dormant OWN rows/parent when its concrete input basis
   arrives. Cold primary issuer runs once; value-only successors share it. No
   retained-value context fold, empty authority or hot full-basis certifier scan.
2. Host owns the SINGLE runtime finalizer/adopter. Source306d separates World basis,
   checks actual original transaction and adds BindingTable-owned predicted witness;
   its early compile is observed by Host. Checked completion first advances PRIVATE
   roots from actual binder/native deltas; final namespace transformation uses SAME
   final prepared promotion after removals/exact-base CAS, atomically with bindings.
   No copied full binding snapshot or caller-derived revision formula.
3. Join genuine three-root/import world, actual protected Seed recipe for BOTH
   value-only cells: x from seed then y from x. Both must demand the import; test
   actual values/effect truth, successor association, prior output deletion,
   predecessor eviction, hot/cold dependency reuse and host work counters. Real
   prologue/expressions/GRE not supported by the first slice must explicitly refuse
   and remain named later migration obligations. Do not invent a passing canary
   target: Qualification/Host must supply its actual maintained production filter.
4. Qualification's birth Rust product7fdb plus same-production1d6c passed eight+one
   controls, including pure Recorded fact lifetime, order equality, modes, real
   native conflicts and bounded changed work. OWN high reviews COMPLETE
   d334/943a→1d6c history. That review found independently composed endorsement
   can pair identical ids/graph with a different native birth owner; Qualification
   must enforce correspondence at publish_original and add a wrong-domain refusal.
   No actual user bypass was demonstrated. Context5→6 and published role native descriptor2/fourroots
   require matching Haskell production reader/profile and genuine Published fixture
   BEFORE campaign fold. Rust-only passes cannot bless the old matched worker.
5. Reload observes c934 readiness-repair retry. Prior986 had35 reports pass and one
   natural completed-startup regression; its source repairfc748 retains truthful
   early terminal intent and guards readiness under actual shutdown owner. Fold only
   complete reviewed repaired history/native. SAME Deployment versus Complete source
   capsule, mount policy and authorized Checking/force successors remain area work.

For a concrete reviewed candidate, inspect full `base..candidate` history and actual
production consumers; responsible parent integrates its area, campaign integrates
reviewed complete histories. Compile joined directly affected production AND cfg(test)
consumers early through supported `swarm-build just build -j1 <exact labels>`, then
counted actual production-path controls. For a wire/profile change, rerun actual
query gate with its matching worker, plus the new ordinary-cell canary; compile-only
or zero matches never count as execution. Publish only reviewable checkpoints:
`git push github HEAD:campaign/shared-warm-compiler`, and collect terminal result.

## Area handoffs

Current durable records are verified for all five supervisors and all nine leaves,
including every completed/unmerged candidate. Completed leaves were reactivated
only for concrete handoffs within existing grants. Exact source/hashes and any
later gaps are retained in private `campaign/HANDOFF-INDEX.json`, updated from
area terminal packets. Current supervisor entry points:

| Area | Current handoff |
| --- | --- |
| Host | `/srv/swarm/checkouts/tidepool-shared-warm-host-inputs-20261019/plans/shared-warm-compiler/host-inputs/HANDOFF.md` |
| Owned inputs | `owned-inputs/SERVICE-SOURCE-CONTINUATION.json`, section `backend_account_handoff` in campaign evidence root |
| Warm workers | `warm-workers/HANDOFF.md` in campaign evidence root |
| Reload | `reload/BACKEND-HANDOFF.json` in campaign evidence root |
| Qualification | `/srv/swarm/evidence/inanna/shared-warm-qualification-20261019/SUPERVISOR-HANDOFF.md` |

 Committed area
handoffs in this checkout describe only their integrated histories; unmerged source
handoffs live in the owning checkout or private evidence, as the index states.
Do not treat an old integrated area snapshot as current unmerged implementation.

## Process/resource and resumption ownership

Campaign has no active external process. Host owns306d compile/count controls;
Reload ownsc934 repaired native controls; Warm owns transport terminal collection
and any next focused controls; Qualification owns its explicit next codec/history
native gate. Check LIVE and exact cgroups again before launching. Idle daemons stay
in global memory. Native admission uses actual global+remaining TOTAL+ADDITIONAL
allowances below80GiB high/96max, physical headroom/Nix and100GiB disk floor plus
all expected growth. Verify bind mounts, use pinned local-only workflow. No broad
cache deletion, daemon restart, cleanup or capacity inference from old samples.

All terminal outcomes must deliver exact task/source/evidence/status/next action.
Producer checks recipient: running→send_message; completed non-root→followup_task.
Auto completion is supporting delivery, not a restart guarantee. No recurring timed
poll/status-only wake. Each process owner observes its terminal result; each parent
reconciles source review and fold disposition. No routine decision waits on root.

After ALL implementation/review/folds and owned tests settle, Qualification owns
bounded scratch analysis by checkout/output owner, active/idle use, independently
required products/evidence and safe reclaim boundaries. Analysis only: no deletion,
unmount, GC or stopping others. Do not call this incomplete campaign complete.
