# Independent checked-row source review

Task: `/root/warm_compiler_campaign/owned_inputs/source_lifetime`, rows Wave 1.
Base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Candidate: `26e0f31df52a04a88dd07913e990b2e9048e9103`, cumulative including
`3716265d2270a0a7ad77556e02a24c998cec2f9b`.
Inspected checkout: `/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019`.

Verdict: repair required before source acceptance. No native compilation or test
execution was performed; the producer's formatting/whitespace checks are not
execution evidence. No builds, daemon changes or resource-policy changes were
attempted. The measured storage blocker and pending admitted commands remain
in `HANDOFF-rows.md`.

## Findings

1. **Bind the promotion slot to its issuing value.**
   `declaration_context.rs:4018`, `capture_checked_value`, accepts an independent
   value certificate and `Option<&Mutex<Option<Arc<RetainedArtifactRow>>>>`.
   The prefilled branch returns the stored row without checking its owner,
   interface/package seals, lengths or requirements against that certificate.
   A valid support/certificate for V2 plus the already promoted slot for V1
   issues a leaf keyed by V2's artifact ID that physically contains V1's row.
   Current production callers pick `produced.output(owner).row` correctly, but
   the owning constructor does not enforce this association; the new concurrency
   test supplies a free-standing slot directly.

   Prefer a private typed output-promotion owner carrying its certificate and
   row slot, with promotion deriving the value from that owner. This also keeps
   independently issued equal-byte outputs separate. Immutable row checks can
   defend the receiving storage boundary without rereading/hashing bytes; raw
   part equality alone cannot identify the issuing output packet. Errors must
   leave promotion uninitialized, and request cancellation must remain a refusal.

2. **Same-view full materialization overlooks newly issued own leaves.**
   `artifact_inventory.rs:6214`, `retain_materialization`, returns an existing
   `Compilation` entry before considering additive `CheckedValueLeaf` entries.
   On a miss it gathers only inherited materializations and likewise ignores
   its own leaves. Thus context V → promote V's leaf on that context →
   `context.prepare_compilation` rematerializes V if no complete entry existed,
   or selects the old broad V row if a V+S complete entry was already cached.
   The source/physical proof remains the same, but the designated immutable
   value row is not used and fresh copying/backing retention survives this route.

   Include current own leaves after inherited sources during complete
   preparation, and define how own leaves overlay a cached complete entry.
   Preserve exact metadata and dependency/group owners. Add both same-view
   controls: leaf before first complete materialization, and leaf after an
   existing broad complete materialization. The current broad-release test
   narrows into a new support view, so it does not cover this route.

## Positive source observations

- `capture_checked` validates module/certificate selection and external byte
  mismatch, then borrows the issuing artifact's stable path and owner. Refinement
  `26e0f31d` uses actual shared `Arc` identity only to avoid comparing an already
  selected immutable byte buffer; independently supplied buffers still compare.
- Promotion precedes existing support projection in ordinary/prepared and host
  binding paths. No extra whole-history refresh helper was introduced.
- Complete versus leaf cache keys are typed. Own/inherited collectors preserve
  ancestor traversal for leaf-only caches, put leaf facts after ancestors, and
  preserve complete-entry suppression. `retain_materialization` itself is the
  remaining missed consumer in finding 2.
- Packet-local locking serializes one successful promotion, and the common
  cooperative lock helper retains cancellation polling while waiting. The
  source has no fallible operation after storing the successful row that could
  return an error while silently losing its owner.
- Checked artifact equality retains path, exact row pointer, receipt,
  certificate, logical compiler support, source/template evidence and prototype.
  Existing strong ordinary/prepared equality remains asserted.
- Compiler input descriptors still enter the existing endpoint transaction
  custody owner. The patch adds no wire protocol, semantic selector, END owner
  or replay policy.
- Required native offer/catalog/prototype owners can retain shared backing.
  The handoff distinguishes them from the value's own leaf; no claim that all
  dependency backing becomes value-sized is made.

## Detection gaps and pending execution

The concurrent test currently calls the low-level helper with an arbitrary new
slot and checks that the returned row pointers match. Exercise the actual
produced-output packet through ordinary/prepared capture (or the repaired typed
owner), and count physical row issuance rather than only observing the winning
pointer. Cover a cancelled waiter while another promotion succeeds and a failed
promotion that leaves the owner empty and releases partial directory/FD owners.

The current broad V+1 MiB sibling test observes old directory paths and weak
descriptor owners after producer/sibling drop; this is a useful physical oracle
for its narrowed-view route. The V→A histories check original A path reuse,
exact V leaf reuse and selection across sixteen independently constructed cases.
They still need execution at the repaired candidate, alongside the mandatory
optional-root/required-child regressions, actual runtime BindingIndex authority
refusal, acquisition-absence controls and a real checked worker/actor history.

All native selected/executed counts in this review are unqualified, with no test
command launched. Source findings were sent directly to rows and the area
supervisor. Next producer action: repair the owning promotion boundary and
same-view materialization route, add their discriminating controls, deliver a new
exact candidate for cumulative independent review, then compile and execute
through an admitted storage lane when one is available.

## Bounded adjacent association search

While rows repairs the findings, independently traced the immediate constructor
neighbors at this exact candidate. No additional confirmed mismatched-owner
constructor was found within this bounded search:

- `HostBindingInterfaceOffer` privately owns prototype, exact request, staging,
  admission, generation and binder. `seal` checks canonical receipt, exact offer
  hash, producer/admission/generation/signature/binder, then actual output seals
  before issuing its checked artifact. Separate caller arguments cannot replace
  the staging owner or prototype inside that consumed offer.
- `CapturedTemplateImports` derives canonical graph and selected support from
  the same original context, requires complete original instance evidence, and
  validates canonical producer/interface identities and graph reachability.
  Its surviving `TemplateImportEvidence` contains graph facts, while required
  physical/native support stays in the checked value's own support.
- Ordinary retained-row materialization constructs each row from the certified
  selected entry, with owned write validation. The new promotion-slot reuse
  branch is the constructor that skips the corresponding association check.
- `original_inputs::encode_image` independently verifies issued transport part
  lengths/seals against its declared part metadata before emission. This is an
  intentional downstream detection boundary. Finding 1 establishes inconsistent
  local row issuance and late refusal/crash risk; silent wrong-code execution
  was not demonstrated.
- Required native-offer custody and original prototype contexts intentionally
  retain their actual original owners. Replacing or clearing them to make a
  physical-release test pass would change authority and is outside this repair.

Transport acquisition-absence, actual FD use through END uncertainty, and native
worker/actor behavior were not executed. They remain separate unqualified
joined boundaries rather than negative source-search conclusions.
