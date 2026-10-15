# Persona self-editing and specialist delivery

## Accepted outcome

Deliver a matched, qualified Persona bundle that supports long-lived notebooks,
Haskell-defined file editing, request-bound specialist replies, and retained
documentation navigation. Preserve authored memory and source when preparing
the next session. Notebook compilation, installed tool execution, source
publication, and inference settlement remain distinct operations.

## Implementation contracts

- Complete session value selections have one opaque, ordered, validated owner.
  Applying an offer consumes the mutable command and produces an applied offer
  with a required selection and immutable physical request. Output placement
  precedes this transition. Certification cannot substitute an absent selection
  with an empty one. Validated retained entries that exclude session injection
  retain an explicit empty selection without fabricating a compiler invocation.
- Optional candidate policy never truncates required compiler inputs. Existing
  byte/work budgets remain separate. Derived program offers retain original
  input selection; generated item interfaces remain produced outputs.
- Preparation refusal uses the existing bounded diagnostic owner and records
  non-submission, exact attempted inputs when retained, and incomplete evidence
  explicitly. Evidence-retention failure cannot replace the primary cause.
- Persona's synchronous read_file, write_file, edit_file, and preview_edit are
  ordinary authored tools. Reuse Persona.Patch and Tidepool.File. Replace/edit
  requires the earlier observed version; preview and commit use the same
  preparation. The filesystem owner issues path/version/durability from the
  actual publication. Never reread to construct a write receipt or retry a
  visible publication whose durability is uncertain.
- Preflight prepares a spec without running handlers, hooks, or startup.
  Ordinary spawn validates before allocation; retained validation is optional.
  Runtime grants and availability are checked at their owning boundaries.
  The actor environment composes its actual intrinsic interpreter support once
  for application workbenches and execution-boundary capture. Validation and
  raw spawn must consume that same composition. Phase-restricted startup remains
  separate; interpreter availability does not grant authority.
- A JSON reply request retains its exact schema and compiled Haskell decoder.
  Prepare the provider schema during reply registration, before logical request
  submission, and carry it with that decoder rather than preparing it again at
  activation. Unsupported schemas must not publish a pending request.
  Validate raw provider arguments before projection and FromJSON, then claim
  settlement. A request's terminal admission excludes unfinished or uncertain
  owned work, preserves completed unread results, and cannot settle another
  activation. A closed activation never invokes a second claim callback.
  Exact operation replay precedes stale-capability checks. A distinct submission
  to settled A may report AlreadySettled while A has no active successor; an old
  A capability presented while B is active must refuse without affecting B.
  The request registry enforces the chosen reply mode at every claim entry.
  Ordinary respond and route callbacks cannot bypass JSON-tool admission for a
  request created with requestWithReplyTool. Its admitted overlay uses a private
  typed entry into the same claim owner; ordinary Haskell requests are unchanged.
- Lookup retains evidence separately from presentation. Direct and exact
  reference expansions share identity-based example enrichment. Search receipts
  distinguish requested, executed, bounded, omitted, and unknown coverage;
  discarded search candidates are not retained display continuations.
  Authored enrichment retains each request's original receipt and view. A refused
  followup cannot masquerade as accepted search work or erase the primary
  retained evidence. Hosted lookup returns a typed capture receipt containing
  its existing display identity; render prose from that receipt, never parse
  prose to recover it. No new display authority or document registry is needed.

## Decisions for implementation

- Keep the useful typed Create/Replace file operation. Wrap it in an ordinary
  object input at the hosted write_file boundary, as required by provider
  function schemas; do not add a schema-normalization exception. Validate the
  entire Persona tool surface before testing individual calls.
- Preserve exact filesystem publication receipts. The changed filesystem wire
  requires one matched Haskell/native bundle and a fresh session; it cannot be
  delivered by copying new Haskell modules into the old running bundle.
- Generic JSON records must preserve every field. The protocol schema owner
  refuses lossy payload-bearing singleton positional sums and object mappings
  with duplicate keys or missing/repeated fields. Intentional transparent and
  nullary encodings remain distinct. Check exact JSON facts, not only round trips
  through a shared defective encoder and decoder.
- Retained lookup composition belongs to the authored capture owner, not to a
  recursive native receipt protocol. Keep primary and related batches immutable;
  navigation preserves each excerpt's issuing identity and view. A typed capture
  result makes the real hosted test independent of incidental display wording.
- Keep the scripted researcher tutorial small and complete. Edge-case lifecycle
  controls belong beside it, through the same real Harness-issued request token.
  Use a custom decoder control to distinguish schema validity from authored
  semantic validity. Do not depend on a live model spontaneously making errors.

## Ownership and integration

| Parcel | Owner |
| --- | --- |
| Applied compiler offers and preparation diagnostics | persona_kaizen_flow |
| File publication receipts and authored file tools | persona_kaizen_perf |
| Prepared spec and preflight | account_switch_review |
| Native request reply and provider adapter | queue_ownership_review |
| Harness terminal admission | response_continuation_owner |
| Retained navigation, enrichment and search receipts | lookup_semantic_contracts |
| Persona.Spec, actor_host composition, compiled examples and joined scenarios | persona_kaizen_api |
| Independent boundary reviews | queue_failure_review and queue_surface_review |
| Release fixture migration and qualification | delivery_coordinator |
| Review, integration and final launch | root |

Workers compile production and test consumers early, execute counted checks,
and deliver commits for independent review. Integrate qualified parcels into
one candidate; do not mutate live sessions or another owner's checkout. Use
warm outputs and measured headroom, preserving 100 GiB scratch and 24 GiB
available RAM. Share interface changes before editing overlapping consumers.

## Acceptance

- Cross 129 distinct retained Val owners through the actual resident path and
  reuse old/new bindings. Check owner cardinality, not just Haskell name count.
  Generated selections cover order, sparse generations, duplicates, invalid
  identities, empty selection, and derived-offer isolation.
- Mutation calibration must detect the restored old cap, disabled semantic
  duplicate rejection, and replacement of the primary refusal by an
  evidence-retention failure. Independently assert selected-owner set equality
  in the actual history. Report executed mutations separately from invariants
  that were only asserted.
- File histories cover stale reads, prepare/commit interference, create races,
  deletion, intentional content restoration, ambiguous/overlapping anchors,
  later-edit refusal without writes, resolved destinations, and uncertain
  durability without replay. Installed file calls after a notebook compile
  refusal must execute with zero additional compiler requests.
- Run a Web-only child through the real Harness with controlled model and Web
  responses. Complete two requests with distinct schemas; separately exercise
  invalid correction, duplicate/stale submission, pending work, cancellation,
  queued next requests, and absence of successor inference after settlement.
- Retained document selection survives source mutation and consumed display
  expansion keys without refetching. Verify complete examples, exact-reference
  attachments, omission receipts, and stale-view refusals.
- Repair release fixtures through the public operations they claim to test:
  authenticated retirement, ordinary-input continuation, and distinct base versus
  synchronous invocation effects. Preserve cleanup and no-effect assertions.
- Qualify one exact matched bundle with the relevant M1-M3 cohorts. Then run a
  bounded real Luna smoke, retaining tool receipts, compiler counts and cache
  usage, before launching the fresh Astra-medium Persona.

## Finish order and ownership

1. Finish the environment-support repair through both public spec validation and
   raw spawn. Run the actual valid-Sync, exact scalar-schema refusal and forged
   installer cleanup controls. Re-run the ordinary callable-reply history, which
   was blocked before it ever reached request settlement.
2. Finish the real shared-machine researcher history, then its dedicated-machine
   equivalent. Assert actual machine/session placement, search/open receipts,
   two different schema-bound requests, and no successor inference after each
   settlement. The API owner is the sole writer of the hosted scenario file;
   the reply owner supplies decoder and lifecycle controls through that owner.
3. Complete invalid/corrected/duplicate/stale/pending-work request histories,
   whole-Persona spec validation, and installed file-tool execution after an
   intentional notebook compilation refusal. Count logical compiler requests,
   including requests served by an already-running daemon.
4. Finish lookup receipt composition and the real hosted lookup fixture. Use
   typed capture identity to navigate the retained content, exact examples and
   refusal receipts; no character-offset arithmetic or rendered-text ID parsing.
5. Integrate source-reviewed parcels, compile the joined production and test
   consumers, and regenerate metadata using its actual owning producers. Use
   source-only integration plus the returned warm delivery checkout; avoid a
   second cold release build. Preserve every existing branch and authored file.
   Verify that exact Harness and shared-workspace revisions are fetchable from
   their canonical remotes; a local object or source override is not delivery.
6. Freeze once the joined source and all pins are final. The final roster must
   contain actual implemented facade cases, not promised tests or regexes that
   select nothing. The 129-owner runtime test remains a separately executed
   joined-source gate because the bundle's facade libtest does not contain it.
   Do not add a test-binary override to disguise that distinction.
7. Run independent frozen cohorts concurrently when aggregate measured memory
   and scratch permit. Use the same retained source configuration for every Buck
   client and honor each cohort's job bound. prepared-child-smoke precedes the
   larger prepared-child cohort. One failure stops its dependent checks, not
   unrelated evidence collection.
8. Stage a fresh Persona workspace from a new stable snapshot of the live
   authored source, prompts and complete memory/notes. Merge the new tools into
   that source; never replace it wholesale with the seed. Preserve the saved
   closing handoff, even though Persona's own write failed. Review the staged
   diff, prepare with the frozen descriptor, run the bounded Luna smoke, then
   launch Astra at medium effort with the selected saved account. Confirm actual
   startup context and readiness before retiring the old run.
   Preserve drafts as drafts: retaining a file is not evidence that it compiled
   or became active. Do not overwrite an authored specialist with the tutorial
   specimen, or claim that the unsaved LookupScout attempt produced a file.

## Bounded overnight repair policy

Continue implementation, source review, counted tests and integration until the
accepted campaign is delivered. Use Sol owners and independent Sol/Luna review;
reuse owners with relevant context. Fix ordinary integration/type/fixture errors
without reopening settled design choices. Preserve the original failing evidence
and strengthen the narrow relationship its test missed.

Stop the affected lane and record a discriminating reproducer if a repair needs
a new compiler architecture, settlement owner, authority bypass, second document
registry, or changed effect-replay semantics. Continue independent work. Do not
weaken typed failures, cleanup evidence, strict schemas, grants or fixture
semantics to make a gate green. Record resource refusals and exact deferred
commands instead of bypassing admission.

The coordinator's private handoff owns current commit mappings, output custody,
commands, receipts and pending selectors. This plan owns the contracts and finish
criteria. Component passes, source acceptance and bundle acceptance remain
separate claims.

## Deferred

No new compiler architecture, patch parser, document registry, automatic memory
policy, or semantic lookup workflow. A pinned package discovery index is a
separate measured experiment. Broader findings need an exact reproducer and
proposal; they do not silently expand this delivery.
