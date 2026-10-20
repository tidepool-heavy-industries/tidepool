# Exact package linking and scoped runtime instances

Root-owned design review, 2026-10-20. Refines `ownership-completion.md` at
the G2 compiler/runtime boundary. Implementation remains split across N/W/L/T;
none of the intermediate checkpoints establishes this joined contract.

## What direct review established

L's `f6c16ca648` retains immutable images in `NativeCodePlan`, but
`PreparedSourceEntry::prepare` discards the resolved import plan.
`prepare_source_startup_entry` later calls `resolve_closed_source_entry_in`.
The shared installer still prefers `code_exports` over target-local package
definitions. Thus image reuse alone does not establish fresh installation.

The existing `package_original_caf_tests` control uses two different machines
and a synthetic HOME forwarder. It proves image sharing can coexist with
independent machine state; it cannot catch an old package instance selected
from the same machine during reload.

The machine-global export map currently supplies package instance reuse across
cells. Deleting it without replacing its lifetime contract would silently create
fresh package state per cell. CAF evaluation, mutable variables and pointer
identity make that distinction observable. HOME's existing scoped instance
machinery already distinguishes code sharing from instance sharing.

N/Q also found that exact historical closure rows were collapsed to logical
artifact/group coordinates. `R8 -> G7old` alongside independently selected `G7new`
may require two dependency occurrences even when immutable code is identical.
That is not unqualified root ambiguity.

## Decisions

1. Immutable code is reusable across installations. Mutable package instances
   belong to an exact runtime selection domain. An isolated replacement B gets
   fresh instances; later ordinary cells in B reuse B's exact instances. A real
   captured reference to A retains A's dependency and refusal behavior.
   Installer scope I and the actor's public notebook scope B are distinct
   runtime domains even when both use B's source generation. Repeated demand
   reuses instances within the actual domain; it does not implicitly transfer
   I's package state into public B. Such sharing requires the existing explicit
   capture/selection transfer. Source-token equality alone selects no instance.
2. Preparation selects the closed dependency graph once. Startup installation
   must not repeat symbol selection, infer dependencies from the current
   inventory, or fall back to a live export map.
3. Graph occurrence identity is distinct from code identity. Exact issued
   dependency bindings determine occurrences; native image keys may share code.
   Each import slot points to its selected occurrence and exported value.
4. Existing lexical custody, atomic batch installation, retirement and rollback
   remain the resource owners. Extend their typed instance identity for package
   code where necessary. Do not add another package registry or pretend package
   code has `CachedHomeOwner`/HOME source authority.
5. A closed entry cannot carry an Existing/live import variant. Ordinary cells
   may carry actual admitted retained bindings and scoped instance leases through
   their existing production path. The two constructors must remain distinct.

## Required dataflow

### N: exact selected completion

Carry the actual input/operation owner, original-use resolution, full exact
group bindings, hidden retained-value requirements and ordered package obligations
as one selected owner. Package obligations name their demander occurrence and
global slot, not just a binder string.

Seal readiness only against actual admitted package products and exact links.
Preserve the association between each full binding and its product/dependencies;
a vector of products beside an unrelated set of bindings is insufficient.
Changing completion must not rewrite an earlier Original's certificate or ID.
Transport may deduplicate identical package preparation requests; it must not
collapse the requesters' distinct links.

Use the existing inventory `ArtifactBinding` for package product plus exact
issued dependency edges, wrapped by a package-kind-checked owner. Existing atomic
scope issuance handles cycles. A demander/root slot is an incoming edge, not a
new package instance identity: two HOME callers of the same exact package binding
in B must share B's instance. Preserve prior bindings when demand grows; do not
introduce completion-wide or per-root graph hashes as another identity system.

### W: immutable definitions and images

Package products retain actual defining bytes/version and full code-affecting
metadata, with ordered import contracts and exact exported values. They grant
code custody, not a source scope or runtime resource authority. Hash and decode
the same captured interface bytes through the existing captured-interface reader;
neither a path prefix nor before/after path observations establish this pairing.

Share images through `ImageRegistry`. Preserve literal backing storage and
DefinitionsOnly entry policy. Product occurrence links remain outside the image
key when they are runtime wiring, rather than code-generation inputs.

### L1: closed link recipe

The sole preparation owner seals a private immutable recipe containing:

- the exact ordered program occurrences and their held images/products;
- each program's complete ordered imports as local occurrence/value references;
- its target occurrence and genuine HOME root/evidence requests;
- the package identities and dependency occurrences needed for scoped reuse.

Private occurrence indices belong to that recipe; no public unchecked index
constructor or re-lookup by symbol. Match representation, entry signature,
literal expectations and export identity before sealing. Recursive edges are
ordinary local references, not a reason to create provisional live exports.
The recipe contains no installed handles, scopes, dispatchers or runtime source
owners. Reject unresolved or live inputs before machine allocation.

Retain this recipe in `PreparedSourceEntry`; replace the temporary ROOT session
and subsequent ambient re-resolution. Do not merely validate it and throw it
away. Installation consumes the held links and images directly.

### L2: runtime binding and scoped reuse

Bind a closed recipe to the newly issued installation domain and actual captured
source origin. Allocate fresh environments/CAFs, then connect already-chosen
edges through the existing atomic batch installer. Publish custody only after
the existing registration succeeds; failure uses its existing rollback path.

Within one domain, an ordinary cell may select an exact installed package group
from the existing scoped custody owner. Selection includes the defining product
and dependency occurrence, not only a module/binder or code-image address.
Newly requested siblings reuse the same recursive-group instance. A different
isolated domain cannot obtain that instance by matching its immutable code.

Factor common physical instance custody from HOME-specific source evidence only
as far as necessary. A typed package branch must not expose a HOME owner through
an optional/default accessor. Keep source authorization exhaustive at consumers.
No second retention table with separate retirement/rollback rules.

First factor the existing physical root (instance, value, handle and signature)
from HOME evidence without changing behavior. Then add an exhaustive package
evidence branch to the same custody/admission path. Toolchain-owned binding
authority stays in the runtime's held selection; codegen receives only the
necessary validated facts and does not acquire a toolchain dependency.

### L3: delete alternate routes

Remove global package advertisement, selected-import fallback, checked-ledger
injection, raw import fallback, prefer-existing installation, export publication
and cache-only durable variants. Preserve actual `BindingEntry` captures.
The deletion is complete only when later cells, not just startup, use the new
owner. Tests of the removed cache must be migrated to the production contracts
they were intended to establish or removed if that contract no longer exists.

Remove the public `PreparedSourceEntry::compiled()` escape after replacing its
actor consumers with narrow warnings/source-selection metadata operations. Runtime
may retain private access. The two native probe helpers currently converting
`entry.compiled().code()` to ordinary startup must use the actual closed source
transaction and installation path instead.

## Work partition and join order

| Owner | Bounded result | Must not claim |
|---|---|---|
| N | Exact occurrence-linked ready closure, real product admission and hidden captures | Readiness from availability or a logical-key set |
| W | Captured-byte product provenance and reusable package images | Runtime instance/source authority |
| L | First exact recipe; then existing custody extension; then global-path deletion | Code-only checkpoint as fresh installation |
| T | Real same-machine lifecycle histories and retained failure controls | Two-machine CAF isolation as reload acceptance |
| Root | Review link/custody types and integrated consumers; own joined G2 decision | Component counts as release evidence |

L first reports the minimal custody type/consumer changes for root review.
Do not start a generic framework rewrite. N/W can finish their independent
producer work while that bounded representation is reviewed. Compile each
receiving contract early, then run the joined history before expanding suites.

## Discriminating acceptance

- Same physical machine: prepare/install A and B from real issued products;
  share immutable images but observe independent runtime instances/CAF state.
- Repeated demand and later sibling demand within B retain B's instance; no
  extra installation resets its state.
- Retire A, then compile and execute a NEW B cell which demands package code.
  B remains callable. A-only explicit captures still refuse where required.
- Exact `R -> G-old` and independently selected `G-new` histories preserve
  both dependency occurrences; a genuinely ambiguous unqualified root refuses.
- Missing/wrong package product or import contract fails before allocation;
  failed registration releases only the attempted installation's resources.
- Closed preparation/installation performs no ambient export lookup or second
  dependency selection. Native image sharing does not substitute for these
  assertions.

Use the existing compiler-produced fixture and hosted source/reload entrypoints.
Keep a low-level CAF test for physical state behavior, plus the actual hosted
A/B/new-cell history; do not force either test to pretend it proves both layers.
Record source/bundle, executed counts and refusal outcomes. Timings follow only
after the joined correctness path is established.
