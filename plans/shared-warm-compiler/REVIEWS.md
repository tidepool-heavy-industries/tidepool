# Plan review record

Status: all six researchers completed cross-review. Root applied the combined
editorial corrections. The final user-requested overnight operating policy
received a bounded follow-up review and its findings were resolved.

Each review covers the root contract, the reviewer's domain, and an adjacent
production seam. Record concrete findings, affected documents, and resolution.
Keep speculative implementation questions separate from contradictory or
missing plan requirements.

| Reviewer | Primary review | Adjacent checks | Result |
| --- | --- | --- | --- |
| `research_incremental_validity` | Selection and dependency correctness across the tree | Worker root identity; qualification oracle independence | Findings corrected and read back |
| `research_structural_sharing` | Physical custody and retained memory | Host root/delta lifetime; worker eviction and END | Findings corrected and read back |
| `research_ghci_sessions` | GHC feasibility and exact worlds | Package universe; reload transitional custody | Findings corrected and read back |
| `research_hls_incrementality` | Incrementality claims and frontend evidence | Host work moved to publication; performance attribution | Findings corrected; final wording corrections applied by root |
| `research_query_lifecycle` | Lifecycle, failure and authority | Callback transfers; cache miss versus accepted uncertainty | Findings corrected; final ownership wording applied by root |
| `research_incremental_options` | Executable multiwave supervision plan | Dependency cycles; shared-file ownership; orphan branches | Findings corrected, including overnight follow-up review |

Root reviews the combined changes and resolves cross-area contradictions before
calling the plan ready. Implementation qualification remains a future task.

## Material findings and dispositions

| Finding | Why it mattered | Resolution |
| --- | --- | --- |
| Candidate use incorrectly waited for publication | Would make reload preflight and ordered private prefixes impossible | C3 and worker01 allow certified private/preparation use; only publication changes public state |
| Unknown-root miss required missing-part enumeration | An unknown worker cannot enumerate a root it has not received; could force a full manifest on every hit probe | Whole-root acquisition on an actual miss; delta only with authenticated known base |
| Worker/input transport dependency cycle | Each area appeared to require the other's finished implementation | Owned rows plus transport contract precede worker realization; actual registration join follows it |
| Scoped package revalidation could bless old EPS objects | Fresh hashes do not change the bytes used by already decoded lazy/native objects | Retain their actual consumed-byte association or refuse/boot clean before user source; add A/B/A control |
| Tiny wire hid full descriptor traversal | Baseline FD/lease enumeration could preserve O(N) work despite zero metadata bytes | Count and refuse hidden baseline part/FD exports on a genuine resident hit |
| Eviction removed only realization roots | Existing global version/recovery/input owners could retain all backing state | Audit those actual strong owners; finite optional retention; confirmed idle-worker retirement fallback |
| Home-membership oracle was underspecified | Both all-custody and executable-closure-only censuses could pass | Independently declare selected-world homes; contrast dormant history with zero-group/interface-only home insertion, removal and forks |
| Reusable input root appeared to contain each operation's demand | Could leak Full/native permission into a later type-only request | C2 separates immutable inherited requirements from fresh operation demand; test both request orders |
| Reload failures implied rollback / all old effects forbidden | Forced partial cleanup changes reality; Continuing source-neutral IO need not be new source authority | Truthful partial stop states and actual completion/cancellation lifetime, with compiler/install/registration fence and callback-transfer stamp |
| Area supervisor could redefine shared contracts | Two branches could independently change authority | Campaign arbitrates; area proposes and owns local implementation/review |
| Repeated preimplementation approval for already specified leaves | Would add unnecessary coordination delay | Accepted bounded assignment authorizes implementation; new shared decisions require arbitration |
| New memory CLI flag became mandatory work | Unneeded to demonstrate sharing on the existing pool | Explicit budget override and large swap/pool sweeps deferred until a measured need |
| Timing names, query output growth and map ownership were ambiguous | Could create false work-removal claims or leave the engine map stale | Offer/admit/seal distinguished from inner phases; fixed-output comparisons; qualification owns per-fold `docs/compiler-work-map.md` update |
| Native collaborators were treated like hosted idle actors | Would add unrelated AgentSpec/request setup to the worker tree | Native collaboration is the default; hosted Exomonad workflow only if explicitly chosen |

The operating-policy addendum also records latest-release refresh, actual remote
identity, model/effort selection, Luna's bounded role, and draining the finite
unblocked frontier before terminating the user's goal loop.

## Review limits

Reviews checked contracts, source entry points, counterexamples, ownership,
sequencing and verification strategy. No implementation or performance result is
implied. The frontend experiment, exact package-source association, quantitative
retention and current speedup remain matters for the specified execution gates.
The peer's later stronger Persona gate was acquired at kickoff: executed1/pass1/exit0,
with compiler cleanup unobserved and diagnostic evidence incomplete; see EVIDENCE.md.

## Integrated host/owned receiving review

Sol-high campaign reviewed host b449..b5 owner-index/deferred-encoding successors and owned full5b74..5113/f330 history, including both baseline-discriminated test repairs. Accepted receiving merges at f088 after critical production/constructor/authority and refusal-path inspection. Private receipt `campaign/REVIEW-host-b5-owned-f330.txt`; joined compilation and36/36 controls then passed independently of area evidence. The user-directed service-owner amendment and required Rust-client migration are
accepted; shared implementation is active under DEMAND-SERVICE.md,
RUST-CLIENT-MIGRATION.md and SERVICE-API.md.

Campaign independently reviewed complete host ebb98..9f086 source history:
fact-only mutation-owner key lookup and bounded producer observations. Catalog
replacement advances invalidation; counter overflow disables reuse; observations
preserve original traversal/policy and count actual encoding before write failure.
Area production/cfgtest compiled and four exact controls passed. Campaign verified
all four report/stream hashes and executed counts; joined compilation is running
at exact e1cfc49, not yet execution acceptance. Private receipt
campaign/fact-key-counter-join/AREA-RECEIPT-VERIFICATION.json.

Campaign also directly reviewed qualification278..e00 native-constructor tracing:
source accepted, existing opt-in full-profile target and bounded observations,
explicit omissions, no policy/authority changes. Actual constructor/frozen failure
attribution awaits native execution; matching interfaces never replace exact issued
historical edges. Independent area review and fresh native admission remain separate.
