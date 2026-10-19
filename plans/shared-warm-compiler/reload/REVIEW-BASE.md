# Reload authority review

Original campaign base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Current joined source: `70f49ac62fb6f3a241fd565e904b2fd043751be5`.
Independent reviewer: `/root/warm_compiler_campaign/reload/authority_review`,
retained Sol-high grant. Supervisor directly inspected canonical owner, source/
target custody, opaque producer/finisher/install, actual recipe caller, ancestry
issuance and the ordinary-cell mutation consumers. Native evidence is in HANDOFF.

## Current source disposition

The complete owned histories have independent exact source reviews. No remaining
confirmed source blocker in these bounded parcels; native/combined acceptance is
separate. Earlier failures and repaired checkpoints remain in Git/private evidence.
This is not full R1, physical END-loss, reload force-drain or release acceptance.

All review packets are under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reload/`:

| Exact parcel | Disposition and receipt |
| --- | --- |
| Canonical lease lifecycle80568, cumulative core519/eaab repair | Source accepted; historical current report is retained in Git. Four owning targets compiled and focused Ready/custody controls ran separately. |
| Opaque producer/actual recipe through7ca and caller fixtures2fec | Source accepted: runtime-targets-review-7ca.md, runtime-targets-fixture-review-2fec.md. |
| Real exact-context fixture029ba90d530a5dd8bb7276b81f2ffa482e9fc863 | Source accepted: target-fixture-review-029ba90.md. Native retry pending; earlier empty-context failures retained. |
| Close evidence985c5eb04620875e83c53d947aece92eeced98e7 | Source accepted: cell-finisher-exact-review-985c5eb.md. Every returned settled failure retains its actual outer close and primary diagnostics. |
| Exact lease ancestry76f5be735b50c01994795cc56fd82839edc124d0 | Source accepted after removed caller and typed placement-oracle repairs: lease-ancestry-exact-review-76f5be.md. |
| Four ordinary mutation consumersb646d7d11cacc9174dc8d0695ac02d4372e8dea4 | Source accepted: notebook-owner-exact-review-b646d7.md. Hooks precede real value/join identity burn; pure branches remain neutral. |
| Durable Sleep positiveeae3a0a58edb227e913fdfcff95298443a883068 | Source accepted: durable-sleep-positive-review-eae3.md. Joined parent input migrated to exact lease at70f; native positive required before relay. |

Current source front doors are nonoptional exact-lease runtime ownership, real
preparation/runtime phase settlement, immutable exact ancestor leases, synchronous
registration under issuer/gate exclusion and matching-owner sealed target products.
No raw gate, raw origin, empty callback or retained bytes grants fresh source work.
The following source details describe the previously reviewed canonical tranche;
latest ancestry/ordinary-mutation changes supersede its old per-file line numbers.

## Repaired source contracts and evidence

| Previous blocker | Actual repair at80568 | Evidence limit |
| --- | --- | --- |
| Pending-to-Ready loses originalA; openB final transaction can refresh A | `resident.rs:2724` Ready holds original ArcA; PreparedInstallMetadata holds target ArcB; off-checkout compilation admits both; final revalidation checks recipient placement and admits originalA+targetB before native/metadata install | New public Ready positive then A-fenced/B-open refusal test is source-reviewed, unexecuted |
| TargetB gate counted through Unknown without exact B scope custody | `source_admission.rs:499` holds strong leases independently of gate claims; every applicable exact lease validates its own issuer/epoch before use and is retained through actual close | New actual-bind/aggregate-Unknown controls cover independent and shared gates; synthetic aggregate uncertainty does not inject physical GHC END loss |
| Static admitted producer checks supplied lease gate but not that lease's epoch | `turn.rs:2728,3065` passes RuntimeCellAdmission's exact strong lease to the same owner, which validates it; final Ready installer likewise checks/adopts exact leases | Complete durable-transfer/stale-epoch runtime history remains unexecuted |
| Metadata record-to-lease-to-owner self-cycle and false physical custody claim | `persistent.rs:359` actual Session owns uncertain holds; RuntimeAdmissionOwner links weakly; each lease retains into its OWN live Session collection; Session Drop seals issuer before clearing holds/native field release | Keeps scopes while the actual Session exists; does not keep a caller-owned Box or compiler process alive after caller Drop |

Gate claims deduplicate by admission same_owner; exact lexical custody deduplicates
by Arc identity. Distinct captured scopes sharing one gate are each validated and
retained. This distinction is enforced in the actual owner, not a test convention.
`source_lease_in` obtains only an existing canonical live target and refuses
MissingTargetLease; it never fabricates authority from ScopeId. ROOT substrate
operations retain their original transaction source; authored notebook ownership
is issued on non-ROOT canonical scopes.

`RuntimeLexicalScopeLease::retain_unconfirmed_source_custody` upgrades only its
own admission owner's weak Session collection. Exact local retirement removes
that scope's hold outside locks entered by lease Drop. `owns_source_admission`
is false for inherited gate records and true for newly issued gates, so retiring
an inherited target does not retire a foreign A gate merely because their numeric
ScopeIds coincide. Session sealing invalidates local issuer aliases and clears
records; it does not claim compiler END or reset unconfirmed counts. No new
registry, gate factory, actor-current reconstruction, or runtime-to-actor edge
was introduced by this repair.

No new lock-order counterexample was found in this delta: admission record
removal releases its lock before uncertain holds are removed; released lease Arcs
drop outside the custody lock; Session seal releases its record lock before
clearing the Session-owned collection. Off-checkout Arc claims do not imply Box
ownership or make concurrent caller Session destruction impossible.

## Existing runtime owner and continuation contract

The private `run_source_compiler_transaction` is the sole new runtime owner entry.
Its opaque borrowed capability checks actual compiler origin and closed state.
Public producers cannot construct or clone that capability. Session entry restores
its prior source context on normal return and unwind before the independent owner
settles actual physical END. Nested raw-origin entry refuses rather than borrowing
permission or settling somebody else's physical scope.

Fresh source allocation, installation, mutation, facade materialization and
selection earn claims lazily through the same owner. Pure precompiled continuation
can enter with retained, fenced source custody without earning a fresh claim.
Actor CompilerWorkTicket delegates once to that runtime owner and supplements
actual actor/bootstrap obligations; it does not wrap another physical compiler
transaction. Panic, abandoned work and unknown close retain uncertainty.

Initial root, program-root, typed child and recovered-root contexts adopt final
canonical lexical ownership after the real preparation boundary. Candidate
preparation does not bind visibility. BootstrapWorld retains the exact canonical
lease; first publication fences that same gate before uncertain IO and binds the
visible generation once. Typed child C has external A plus own C; replacement D
retains A, not C. Fresh/captured independent LLM worlds have their own issuer.
SourceCallback/from_context aliases preserve their original immutable dependency,
including generation metadata None; recipientB cannot relabel sourceA.

Unknown initial publication leaves old bootstrap aliases unavailable. Recovery is
explicit host/run reopen selecting actual durable head, references and readiness
into a NEW context. No same-actor live reconciliation, prospective public owner,
new same-ID gate, or reuse of old unavailable aliases is accepted.

## Actual physical custody and accepted limitations

Compiler close evidence retains exact input File/TempDir Arcs and, on unconfirmed
direct retirement, the exact Child handle. Daemon quarantine retains its Worker,
namespace and resource/admission grants. Registry slots or the checked-out blocking
task own Box<ResidentSession>; metadata RuntimeAdmissionOwner does not. The task
retains Box plus checkout receipt even when its async waiter is cancelled.
RootCustody/BindingLease account concrete external roots and notify existing child
teardown; a lexical Arc alone does not retain registry membership.

The actual actor CompilerWorkObservation retains close evidence. A standalone
public caller can supply a no-op sink and discard CompilerTransactionSettled;
that is not permanent compiler-process/input custody or proof of reaping. There
is no demonstrated live unknown-to-confirmed reconciliation consumer. The current
borrowed public contract cannot keep an entire caller Session alive after caller
Drop. Stronger operation lifetime semantics require an actual owned-operation
contract, not metadata self-retention or a claim that Drop confirms compiler END.
These limits are explicit in the leaf packet and remain unqualified behavior.

## New controls and remaining acceptance work

Source-reviewed public controls at80568:

- `ready_install_keeps_original_source_when_recipient_owner_changes`: real
  snapshot/off-checkout/final install positive under open A, then A-fenced/B-open
  typed refusal before exports/table changes; B remains open.
- `runtime_source_owner_retains_each_exact_scope_until_its_session_retires_it`:
  actual public owner and binding producer; independent gates and same gate with
  distinct scopes; injected aggregate Unknown; last target lease drop and actual
  reap preserve binding; explicit target retirement releases only target hold;
  Session teardown seals issuer and leaves no admission metadata self-cycle.
- Public facade fixture preserves a meaningful empty root exact context as None;
  nonempty capture still checks exact Arc identity and semantic SHA. This is a
  fixture repair, not permission to substitute a different compiler owner.

Deferred required controls include actual stale public-owner transfer, foreign
inherited gate with coincident cross-session ScopeId retirement, originalA last
external lease drop before its own Session reap/teardown, and actual physical
post-resume GHC END loss/panic/cancellation retaining exact input and lexical
custody. Existing new aggregate tests do not establish those physical faults.

Known unaccepted successors remain:

- Public standalone `runtime/lib.rs::run_compiled_target` newly reached by the
  facade fixture requires real issuer/owner migration. MissingTransaction is not
  repaired by a test-only context wrapper or a permissive compatibility adapter.
- Detached declaration validation and old eager actor ticket consumers/fixtures
  must join the actual owner. The common published-session compiler client must
  serve queries, ordinary cells, successors/fork/reload/recovery off checkout;
  every actual operation/warm hit retains original owner, END/cancel and stale
  publication checks. No machine checkout while waiting, actor reconstruction,
  or actor-current registry is accepted.
- R1 WatchRoute/forms/reply-tool/display registration coverage remains required;
  broad R2 fence/publication/force drain and R3 are held. Existing directory
  reservations and workers preserve typed dependency and cleanup uncertainty.
- Baseline quoted-source exact-selection roundtrip and NativeGroup count failures
  remain separately classified; do not weaken identities or infer shared cause.

## Durable evidence and next event

Leaf packet:
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/reload-admission-20261019/c6-exact-lease-repair/HANDOFF.md`.
It retains exact delta/checks and deferred owning commands. Leaf native counts0.

Cleanup-owner trace:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reload/c6-existing-cleanup-custodian-trace.md`.
Baseline classifier:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reload/quoted-baseline-owner-trace.md`.

The former2126-line checkpoint chronology is retained in Git at area
`03699b38bab63b248e49be30a143775ef7a68caa:plans/shared-warm-compiler/reload/REVIEW-BASE.md`.
It is historical evidence, not current disk admission, test counts or acceptance.

Next producer: reload owns exact candidate fold for compiler feedback, actual
production/cfg(test) compilation and focused counted controls; generation_admission
owns bounded repairs. Any new candidate requires exact rereview. Reviewer awaits
that concrete source/evidence event; no periodic polling or new analogue search.
