# Consolidated G2 review checkpoint — 2026-10-22

## Source and current status

Authoritative checkout:
`/srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020`.
Branch: `refactor/g2-linked-code-ownership-20261022`; public remote `github`.
Code candidate: `c92fb6217bac9774836452cc6c32f7e062263a2e` (bounded test correction over `536be83679`).
G2 and G3 remain **unqualified**. No matched bundle or new Persona run exists.

The user requested consolidation before another planning pass. Finish the
bounded current validation and record terminal evidence; do not automatically
launch another repair/refactoring wave. Preserve authored Persona files and
memory, and do not launch a stale binary or generic profile.

## Current repair and why

The actual staged World history first exposed unused lexical reconstruction in
planned declaration validation; that traversal and its discarded adapters were
removed. Its next failure was a retained-template completeness refusal.
Scoped diagnostics at `03c5eaefd2` established the precise cause: item 4,
generation 3, stage 4 selected Val.G2 but inherited a Val.G1 instance obligation.
World and finite value publication both inserted synthetic Val lexical nodes,
turning cumulative TYPE availability into requirements for later compilations.

Integrated repair `80222d4de8` (root candidate `175c1d1e7c`):

- Both publication paths retain certified TYPE dependencies without adding Val
  nodes or nominal TYPE edges to the accumulated lexical graph.
- Original-output admission uses the actual preparation's checked-value authority
  and matching selected thin interface to issue an instance-empty leaf.
- Missing selected checked interfaces now refuse explicitly; wrong interfaces
  still refuse. Actual GHC thin interfaces start from `emptyFullModIface` and
  define no instances. Canonical source/orphan adjacency remains separate.
- SOURCE receipts, native demand and the original completeness guard are intact.

Astra independently source-accepted the exact production patch and inspected
both finite and World consumers. Root reviewed and integrated P's component
repair `536be83679` (candidate `e24b9b68b2`), which removes fabricated Val SOURCE
rows and checks selected, unselected, missing and mismatched TYPE inputs.
The real staged history additionally asserts G1 → G2 selection and absence of
obsolete G1 obligations. Component fixtures are not actual GHC qualification.

Review evidence:
`/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/reviews/import-admission-boundary.md`.
Exact pre-repair diagnostic:
`/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/linked-03c5-template-stage-context-checkpoint.json`.

## Verification

At `536be83679`, toolchain/runtime production and affected unit-test executables
compile successfully (four targets, 14 local actions). Compilation is not a test
pass. At that exact revision:

- Staged runtime 0/1: all item sealing and template captures finish; the test then
  fails at the new selected-value lexical-leaf assertion before effects run.
  Root and Astra traced a test-premise error: same-cell prior-stage values use
  ProducedValueTypeInterfaces, not the checked-input authority. `c92fb6217b`
  changes only this assertion to inspect actual selected TYPE roles. Its final
  bounded rerun at `c92fb6217b` fails 0/1 after 78.14 s with:
  `Session(Compile(ExtractFailed("duplicate selected lexical owner main:Tidepool.Session.Lib.G1")))`.
  All preparation/capture and callback TYPE-role/native-demand assertions clear.
  The initial `execute_observed` returns this later failure; full effect-history,
  published-binding and fresh-consumer assertions remain unexecuted. Runtime
  test compilation passes at this source; no production change followed 536b.
  Locate duplicate issuance before choosing a repair: the two refusal sites are
  `ExactLexicalSnapshot::build` in `exact_context.rs` and declaration-context
  validation. Do not silently deduplicate possibly conflicting adjacency.
- Component batch 2/5 passes: private nominal TYPE owners and interface-only
  custody/source/native separation. Three failures are itemized below.
- Actual quotation/template control fails before compiler submission: its test
  removes DAEMON_SOCKET_ENV, conflicting with required owned-native compilation.
  V also attempted direct mode; the rooted package-service authority correctly
  refused it. Neither attempt executes the quotation/template checks. Do not
  bypass that requirement or report these as template product failures.

Remaining component failures, source diagnosis versus observed evidence:

1. `selected_original_instance_graph_keeps_mounted_value_without_public_lexical_edge`
   reaches its final successor `.is_ok()` assertion after the selected/unselected
   and missing/mismatched interface controls. The continuation preserves checked
   authority. It instead replays a FreshSource receipt against the context already
   holding its generated Consumer Original. Source tracing predicts
   HiddenOriginalPromotion at OriginalExecution; the `.is_ok()` suppresses the
   actual error envelope. Fix the request/history premise and retain that error,
   rather than weakening the production refusal.
2. `program_support_preserves_interface_only_receiver_dependency_without_native_authority`
   unwraps absent `program_support` before its new lexical assertion.
3. `program_support_refuses_fresh_value_type_without_reserved_output_authority`
   makes the same assumption. The owner returns the updated TYPE context before
   creating ProgramSourceSupport when admitted lexical roots are empty. These
   fixtures assume TYPE admission creates SOURCE support. Baseline execution was
   not performed, so this is a source-level premise diagnosis, not a bisected
   regression claim.

P's retained read-only analysis is
`/srv/swarm/evidence/inanna/g2-linked-code-ownership-20261022/checked-value-instance-successor-536b-analysis.json`.
Astra's report corrects its earlier acceptance of the staged assertion. No new
product authority is justified by these fixture failures.

All commands, source pins, artifact/resource hashes, actual case counts, primary
errors and cleanup limits belong to V's retained records under:
`/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/`.
Final retained summaries:
`validation/STOPPING-CHECKPOINT-c92f.md` and `validation/linked-c92f-checkpoint.json`;
component and quotation details remain in `validation/linked-536b-checkpoint.json`.
Do not combine ancestor counts into current qualification.

Relevant earlier evidence:

- `acd6ea2259`: planned declaration/custody plus fresh package-path controls 4/4.
  Recovered immutable bytes and a fresh acquisition's ambient-path checks are
  separate contracts. No repeated execution needed unless those owners change.
- `1d5c7d86f8`: Context9, carried World TYPE/global names and mixed World sealing
  each pass their recorded single case. Runtime staged history was still failing.
- `0197741519`: real checked-value closure history passes all four combinations
  inside one counted case, including explicit/inferred SOURCE and ordinary/
  delayed injection. Other retained controls have their own source pins.

## Worker/parcel disposition

Current production, diagnostic, test and map corrections are consolidated;
there is no current unmerged repair awaiting review. The failing component tests
remain in the consolidated branch for explicit repair; do not call it green.
Astra and P are stood down
with retained reports. V completed bounded validation and released the source pin. All validation
clients settled; process and owned-compiler cleanup are confirmed, hosted cleanup
was not observed, and full diagnostic completeness is not claimed. Root owns
this consolidated handoff and push. No worker is currently assigned a new repair.

An older optional custody-retention discriminator (`85b3f8ffbf`, also recorded
as `a498ba26d1`) remains preserved, uncompiled and unexecuted. It concerns
A/B → A retained memory, not an established new authority defect; do not silently
count it as complete or add it to the release gate. The obsolete diagnostic
`d21d74b242` targets a retired representation and should not be integrated.
Prior-source adjacency widening remains an unproven review lead, not a confirmed
release blocker. Evidence: `reviews/import-policy-parity.md` in the directory above.

## Next planning pass

Review the joined dataflow before choosing the next bounded wave:

1. Can cumulative availability and exact current compiler obligations have
   distinct owning types, so publication cannot inadvertently create future
   dependency requirements?
2. Which broad context-mutation APIs still conflate nominal TYPE, source
   permission and original instance adjacency? Preserve real inherited instance
   closure; do not solve missing evidence by dropping inconvenient graph rows.
3. Which test fixtures reconstruct roles the actual producer never emits?
   Keep independent refusal controls and a small production multistage history.

These are questions, not an authorization to introduce another framework.
The current owner map is `docs/compiler-work-map.md`; active contract and review
questions are in `plans/shared-warm-compiler/completion/linked-code-ownership/next-review.md`.

Remaining G2 gates after the bounded repair are recorded with exact commands in
`/srv/swarm/evidence/inanna/g2-linked-code-ownership-20261022/c-remaining-gates-f5e8.md`:
prior-World publication/reuse, source-proof quartet, actor Text/acknowledgement,
ordinary notebook/publication/custody/query/reload/cancellation/uncertain-END/
historical recovery and affected corpus checks, then one matched catalog/bundle,
four named Persona cases, release contracts and selected-spec/tools/memory plus
real DeepSeek turns. Its f5e8 status is historical; use this handoff and current
V records for the current verdict. Full G3 force/fork/callback/process-recovery,
M1–M3 and performance remain separate.

## Operational constraints

Build through pinned, local-only `swarm-build just ...`; investigations through
`swarm-command`. The warm joined checkout has a registered Buck bind mount.
Last observed scratch was about 117.7 GiB; preserve 100 GiB. The bounded warm
tranche forecast is at most 3 GiB. Do not mutate source during V's pin or delete
active outputs. Preserve sources, Git history, WIP and evidence.
