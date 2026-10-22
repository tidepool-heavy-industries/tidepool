# P/H: preserve exact selected support through the join

Primary outcome: ordinary and World-item compilation carry exact native/package
choices into runtime installation. Secondary: remove reconstruction, avoid
duplicated graph work, and replace shallow oracles with the next real consumer.
Keep the working occurrence-table format and existing graph owner. This updates
the earlier P/H assignments after review of `5c759554c6`.

## H: close support only after its required constituents are present

The focused Job rerun fails in
`RequestNativeAvailability::retain_acquisitions`, `declaration_context.rs:2035`.
The absent native payload is `Tidepool.View.Types`, required by a prior
`Tidepool.Session.Lib.G1` group. Its compiler role remains InterfaceOnly; that
is correct. Haskell plan and physical preparation succeeded.

Source trace identifies a concrete composition seam in
`artifact_inventory/compiler_projection.rs`:

1. `project_world_stage_inputs` calls `project_demanded_inputs` with deliberately
   empty prefix native groups; the prefix owns compiler roles, not a new native
   selection.
2. The latter narrows custody from namespace plus those group roots, carries
   native availability/completions, then calls `SupportDraft::close`.
3. Only after that call succeeds does the World path add the independently issued
   initial stage native support. Required dependency custody can be lost before
   this addition is reached.

Retain both issuing inputs through this composition. Use the existing private
`SupportDraft` boundary to assemble compiler projection plus exact stage-native
custody/availability, then close once. Defer only physical completeness: check
the prefix's own demand/source identity, unchanged role bindings, permission
delta and exact group permissions before borrowing stage custody. A later stage
cannot repair a prefix's missing permission. Authenticate the exact stage even
for empty or metadata-only selections; stages may share request/operation and
initial basis, so those equalities alone are insufficient. Keep prefix namespace
decisions separate from the independently issued stage executable choice.

The ordinary path still closes its own complete input with all current checks.
Do not publish an incomplete support as
a closed value, weaken `retain_acquisitions`, retain every available payload, or
promote View.Types into the Original compiler namespace.

Validate this diagnosis against the retained trace as implementation begins; the
exact throw/root is executed evidence, while its earlier dropping call was
identified by source tracing rather than a stack capture. The adjacent
`source_support` constructor has the same closure obligation: native completions
must not outlive their required custody in a narrowed support. Inspect that
caller too and remove duplicate reconstruction once issuer-led composition
suffices. No new public draft API or second artifact registry.

First controls: the real declaration/value -> first Job carrier history; selected
dependency custody under InterfaceOnly; add/reorder an unused same-interface old
Original and preserve the selected result; real missing required payload still
refuses. Also refuse same-request stage substitution with empty selection, wrong
prefix permission with otherwise valid stage custody, and a partial selected group
plus an unrelated available sibling. Include Text/JSON at the same boundary without recompiling identical
fixed fixtures needlessly. Keep the existing exact historical-edge refusals.

## P1: carry the selected row, do not recover its token afterward

Three facts stay separate:

| Fact | Existing owner / meaning |
|---|---|
| Content identity | Payload / CachedHomeOwner hashes permit byte sharing |
| Exact issuance | NativeGroupBinding and issued selected row own exact imports/history |
| Current source membership | Actual selected source receipt proves this row belongs to current installation |

Existing machinery already carries the needed choice:
`ChosenNativeGroupSupport`, `CurrentProductRef::available_source_dependency`,
`CompletedNativeProductAvailability::choose_own_group`, and
`IssuedSelectedNativeGroup`. Reuse it.

The physical cohort issues a fresh row once; multiple targets of that same
cohort borrow it. An imported/held row comes from the exact chosen support and
exact completion lookup. Carry that row and its ordered imports through certified
groups, completion and source products. Stop flattening it into independently
reboxed products and later trying to rediscover the original token.

Delete the owner/ordinal scan across all inherited completions in
`complete_native_selection`. Equal CachedHomeOwner includes content/version
facts but not issuance scope. Neither equality nor equal completed imports
authorizes picking an unselected historical row. Keep validation of a prior
selected by its actual owner and the genuine same-selection conflict refusal.

Keep source membership independently authenticated. A held row is not made
physical-current by sharing bytes or being reused. When current receipt and
selected prior have a legitimate association, carry that exact association;
do not silently substitute its product only in completion while the runtime
source consumer still holds a different token. Preserve distinct environments
and distinct physical cohorts even when their content is identical.

Earliest acceptance must continue through
`PersistentSession::resolve_native_turn_in` -> `NativeInstallationSources::current`
-> `install_native_turn_in`, observing actual site/source custody and execution.
Arc equality in a completion-only test does not establish this. Include a crossed
source receipt and an unused equal-content environment. Neither can authorize
current source or veto the exact selected environment.

## P2: one admission policy for paired ordinary and item products

The item reader still filters HOME rows to `ProductOrigin::Fresh` even though
the producer and full decoder now emit/admit demanded cached HOME roots. Replace
that split policy with one issuer-scoped resolved HOME coordinate/count used by
both consumers. Keep item/stage identity, ordinal, ordered slot count and range
checks. Do not suppress the cached roots to evade the refusal. RetainedCore
coordinates are legal only where their real producer supplies that authority.

Ordinary `BoundPackageSelection` currently stores a program digest that no
ordinary consumer checks; one caller supplies zero. Remove that ceremonial proof
shape. Authenticate the actual prepared target and its ordered imports at the
existing constructor, and carry program plus bound selection inseparably. Admit
globals and roots from one closed receipt target reference. Remove independent
selection constructors and separate program/selection completion arguments,
including crate-private routes; the existing bypass is already crate-private.
Separate transported item files still require
their real wire hash and stage/item checks. If a digest is retained for an actual
cross-file comparison, its production consumer must check it; never fabricate it.

Keep the existing occurrence closure and graph issuer. A missing manifest is not
empty demand. Separate independently issued environments; do not add recursive
hashes, global equality normalization or a new parallel graph implementation.

## P3: return reached HOME demand from the existing walk

The producer filters HOME groups using only binders directly referenced by the
final target, while `OriginalProductRoots` already walks transitive HOME edges to
find package leaves. Return the reached HOME coordinates/import obligations from
that existing worklist too, scoped to the target and its actual successfully
issued current/cached products. Pass owner-scoped references with ordered imports,
not bare coordinates for a later rematch. Retained/prior groups keep their exact
existing completion route. Feed only that target's closure to the existing projection.
Do not reproject every HOME body for every target or add another full-graph walk.

Discriminator: NOINLINE target -> HOME A -> HOME B -> package code, with B absent
from the target's direct globals. Assert the intended non-inlined edges from the
real producer before testing the result. Both fresh and cached forms must retain
the demanded ordered package roots; an unrelated unsupported body is not demand.

## Delivery and review

P1, P2, P3 are ordered integration cuts under one native/package owner, not three
competing edits of the same consumer. P3's Haskell implementation can proceed in
parallel after P freezes its returned coordinate contract. H repairs support
composition independently in named functions, then joins P before actor gates.

Tests enter through the current declared producer and actual World/native/runtime
consumers. Keep the useful nine-case cohort, strengthen its claims, and do not
replace actual authority with raw synthetic certificate construction. Mutated
wire bytes are appropriate for refusal controls only.

After each cut inspect the next consumer: closed support, native completion,
source installation, next cell, publication and disposal/reacquisition. Record
source-derived concerns separately from executed counterexamples. A later error
means the previous frontier was crossed; it is not an overall pass.
