# G2: shared offer issuance, input HOME contracts, and bounded diagnostics

## Accepted boundary

Base code: `bff9e561755246ace2fe8ff139441844c4067a04`.
Base documentation: `d0b92b3347` on `integration/g2-domain-runtime-20261023`.
The parent integration checkout is
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Prior review and evidence: `g2-source-owner-mid-effort-review-20261024.md` and
`/srv/swarm/evidence/inanna/g2-source-owner-20261024/`.
New evidence: `/srv/swarm/evidence/inanna/g2-offer-issuance-20261024/`.

Argon identified independent defects: separate requests allocate new ordered
import owners for one available HOME group; preview package projection omits
the HOME-to-package contracts of previously unlinked input Originals. Fix the
issuing and transfer boundaries, preserving exact identity and authority.
SOURCE fixture admission and diagnostic amplification are separate findings.

All workers use isolated clean checkouts/branches, retain previous commits, and
compile changed production and test consumers before delivery. No ambient tool
substitution, broad cache removal, manual proof surgery, or permissive fallback.
Record exact source, executed counts, commands, retained causes and cleanup.
Read the assigned revision's contributor/build guidance. Use actual resource
headroom for independent builds. Root owns integration and critical review.

## A: shared cohort rows and acquired HOME index

Owner: existing G2 Rust worker. Paths: native_availability.rs,
compiler_projection.rs, linked_native/restore.rs, and the row certification /
demand assembly regions of certified_products.rs and declaration_context.rs.
Coordinate shared files directly with B; neither edits the other's regions.

- Replace validation-only offer scope checking with a checked issuance that
  retains one row per exact NativeGroupBinding across the whole cohort.
  Seed from actual physical/issued rows and held input rows. Subsequent scopes
  must reuse the same Arc after checking the product and ordered destinations.
- Offers retain immutable reachable subsets of the shared rows. Narrowed
  contributions check exact membership before lending a row. Availability does
  not select executable roots or bind runtime values.
- Demand consumers use contribution-owned rows together with existing held and
  local rows, refusing conflicting issuers rather than choosing whichever wins
  map insertion. No independent per-offer reallocation of a shared child.
- Cover every issuance/restore route. Restored offers and acquired closures in
  one recovered context must share authenticated rows; do not silently retain
  a constructor that can omit them. Serialize durable facts, not allocation IDs.
- Add an immutable derived homes index to AcquiredNativeClosures, analogous to
  its packages index. Construction and append check same physical issuance and
  linked code for each exact HOME binding. group_owner becomes a charged lookup.
  Preserve cancellation/budgets, no new default operation or global interner.
  Same-owner comparison is an explicit domain operation; verify actual existing
  types rather than copying illustrative names from the review.
- Tests through production owners: two independent demands from one unacquired
  Full offer, overlapping offers sharing a child, both completion orders,
  restrictions, restored overlapping closures, genuine conflicting issuer/code
  refusal at append, and subsequent successful demand. Availability-only
  publication must leave executable selection unchanged.

Send B the exact checked row-access API early. Escalate an issuance cycle or a
need for broad eager materialization before adding caches or a second authority.

## B: input Original HOME-to-package contracts

Owner: existing Q6 worker. Paths: ExecutionProjection.hs, CertifiedProducts.hs,
CompilerRequest.hs package-target assembly; certified_products.rs HomeMembership,
ValidatedHomeContracts, BoundPackageSelection; focused preview/contract tests.

- Carry reconciled exact input Originals into package target projection.
  Emit ordered package-slot contracts only for reached exact HOME groups. Include
  their authenticated coordinates in receipt validation. Do not reintroduce
  whole-inventory executable demand or flatten versioned identity.
- Add explicit InputOriginal membership retaining the actual authenticated group
  owner, not merely a module/ordinal or caller-composed identity fields. Derive
  correlated facts from the owner where possible.
- Issuance requires Input origin, bound original/canonical identity, exact
  owner/version, permission for the ordinal, and exact ordered package slot
  identity/interface checks. An arbitrary contribution containing an ordinal
  does not resolve competing exact issuers: use the request's actual choice.
- Consumption joins the historical source association and exact group/product
  owner. Coordinate A's row ownership API; do not use raw Arc pointer equality
  between wrapper allocations unless the owner guarantees that allocation.
  Preserve the existing same-issuance domain check where it is the contract.
- Positive: real activation preview/LinkHome whose previously unlinked input
  HOME group imports PACKAGE code. Retain the full rendered-value assertion.
  Refusals: wrong product/version, missing/extra/reordered slots, bad package
  interface identity, and a partial offer omitting the ordinal.
- Purge obsolete projection signatures and migrate all callers. Run affected
  Haskell component checks and the real runtime preview case; coordinate the
  compiler translation/serialization fixture gate with root.

## C: bounded diagnostic production and overflow evidence

Owner: existing G2 Haskell worker. Paths: daemon.rs trace profile, GhcPipeline.hs
native-use logging, WorkerServer.hs capture/render lifecycle, focused tests.

- Full profile defaults to debug and reports debug. Explicit TIDEPOOL_LOG
  remains authoritative. Census is debug; per-identifier details remain trace.
  Do not add unsupported target-filter syntax or change semantic discovery.
- Keep the 16 MiB synchronous response frame limit and typed capture-budget
  refusal. Rejected capture carries a bounded diagnostic sample and measured
  sizes through the existing response/evidence path; it never exposes Ready.
- Sample stderr head/tail without reading the whole file. Bound by the actual
  remaining budget, reserving space for failure diagnostics and a truncation
  record; tiny/zero budgets must still yield a bounded failure. Handle overlap,
  invalid UTF-8, file growth, read/seek failure and cancellation honestly.
  Never sample stdout as a successful machine response.
- Preserve primary/cleanup failures and temp-file release behavior. Tests enter
  through the real capture/render/protocol path: over-limit and tiny-budget
  refusal, distinctive head/tail evidence, no Ready, wire ceiling, cleanup,
  ordinary success and explicit trace override. No new diagnostic side channel.
- The measured failing Q10 stderr size is 16,974,750 bytes. The trace-category
  split is an estimate, not a parse of the discarded stderr. Rerun the named
  ordinary history after integration; attribution still requires its second cell.

## D: SOURCE fixture and refusal controls

Owner: separate Sol worker. Own source_dependency.rs tests and adjacent fixture
files only. Coordinate with A if production construction needs a row accessor.

- Read the failed fixture reports first. Do not import a generated Lib.G* from
  an ordinary authored disk module or splice a later support into older custody.
- Use real CompilerWorld progression: an executed bound value, then a declaration
  capturing it, followed by the complete matching declaration context. Derive
  actual generations and identities; do not assume `let` produces a Val module.
- Confirm Full issuance, nonempty exact retained dependency, and SOURCE admission
  independently. A declaration's existence does not itself prove SOURCE rights.
- The proposed separate_native_offer_custody helper is not present in accepted
  source. Find the existing production owner transition before implementing any
  fixture helper; no test-only method that edits roots or drops grants.
- Observe an exact Full-offer group absent from the observer's passive view,
  then consume its nonempty retained terminal, with selection unchanged.
  Exercise partial-offer denial, unresolved competing issuers and out-of-scope
  retained occurrence through the actual SOURCE consumer.
- If the proposed relation cannot be produced through supported constructors,
  stop with a minimal exact explanation and a production-valid alternative.
  Do not spend multiple rounds inventing increasingly artificial fixtures.

## Review and integration

One independent Astra review checks A/B's shared ownership boundary while they
work: overlapping offers, restoration, exact input group choice, and whether the
new types prevent row loss without forcing unnecessary work. Review is read-only;
root resolves decisions and reads the critical diffs/consumers directly.

Integrate reviewed compilation-ready parcels into the campaign branch promptly,
then run combined focused tests and the real actor/preview/ordinary histories.
Keep code and evidence source identities separate. After corrections, give Argon
an exact-candidate adversarial handoff including purge checks and remaining gaps.
Do not claim G2 from local controls. Frozen bundle/Persona qualification follows
the remaining g2-completion.md gates on one integrated source/asset contract.

The bff9e56175 actor rerun has four settled receipts (3 pass, 1 same HOME conflict)
and an interrupted runner with a fifth case lacking a terminal receipt. Its
trailing `Killed` does not establish OOM or cleanup; root will audit surviving
ownership before reuse. Do not count that fifth case as passed or failed.
