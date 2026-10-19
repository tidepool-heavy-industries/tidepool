# Actors use a compiler service

Accepted user-directed architecture amendment, 2026-10-19. This is required
implementation work in this campaign, not a proposed follow-up. It controls
older instructions that permit retaining the old Rust request construction as
the normal route or treating a compiler-world reference as optional acceleration.
Read with [compiler-owned worlds](DEMAND-SERVICE.md) and
[the first service slice](SERVICE-API.md). Implementation status belongs in
[STATUS.md](STATUS.md); this document establishes no passing result.

## Outcome

Rewrite how actors engage with their compiler. An actor asks its compiler about
its published world, supplies actual edits when preparing a cell, and consumes
the returned answer or prepared program. It does not assemble a compiler universe
from retained artifact blobs to formulate each question.

The first public-module query is the first integration milestone. Completion also
requires ordinary notebook cell preparation, incremental successor publication,
fork/reload/recovery, and deletion of the superseded Rust reconstruction routes.
Do not report a query-only cache as completion of that migration.

## Identities and ownership

Use an opaque, service-issued logical world reference behind the existing
toolchain capability. Its concrete type name is an implementation decision.

| Fact | Owner and contract |
| --- | --- |
| Published notebook and current logical world | Existing Rust session/publication owner; bindings, source selection and world association are one coherent publication |
| Logical compiler world | Service-issued immutable identity binding exact primary source, lexical selection, profile and explicit implementation choices; neither actor ID nor process ID |
| Live realization | Service-private incarnation-bound state; eviction/worker rotation can replace it without changing logical world identity |
| Restore product | Versioned, authenticated immutable facts owned with their backing; preserves the exact world and historical references without running cells or replaying effects |
| Runtime values and resources | Rust runtime retains values, grants, executable provenance and exact issued dependencies; possession of a world is not execution authority |
| Derived compiler knowledge | Haskell service owns resolution, checked modules, types, instances and dependency invalidation under its actual GHC ownership constraints |

Prefer extending current session/toolchain capabilities to adding a new registry
or capability framework. The actor-facing facade obtains its world from the
published session owner. Ordinary callers must not manually pair a world ID,
source version, value inventory and compiler profile. A naked ID on the wire is
a locator, not proof or authority. Missing residency is an internal acquisition
condition; it is not a missing notebook world.

Keep runtime and compiler concerns distinct without making callers reconstruct
their relationship. The existing source admission and END/cancellation bracket
encloses every actual service operation, including a warm hit. Do not hold a
machine checkout while waiting for the compiler. Results retain their exact base;
publication uses the existing stale-writer check and atomic owner.

## Required request shape

Names below describe operations, not mandatory new endpoints or tool names:

```text
published session -> scoped compiler client

inspect(world, bounded question) -> located typed answer
prepareCell(world, authored cell + actual changed inputs)
    -> prepared program + certified outputs + private successor description
publish(existing admitted execution, outputs, successor)
    -> publish the admitted item, or refuse it while retaining prior committed items
restore(exact retained world product) -> same logical world, new live realization
```

Inspection is observational: hydrating a previously undemanded module must not
change actor bindings, primary selection or logical world identity. Exact known
type evidence can answer without a worker call. Arbitrary expression checking
may require real GHC work; do not add a separate mandatory pre-check before the
existing whole-cell preparation. Compilation precedes effects as today.

Preparation requests contain authored source, actual binding/import/source
changes and references to required owned inputs. They do not contain rebuilt
all-values support graphs, a full compiler inventory, or new serialized baseline
images on every call. The service follows exact references and determines the
necessary compiler work. Runtime authority admission remains explicit and fresh.

The service returns the answer or prepared program that the caller needs, plus
the typed evidence needed by certification/publication. It must not export its
entire derived environment for Rust to reconstruct the answer or next request.
Prepared artifacts still cross the boundary when the JIT needs them. The change
removes unnecessary knowledge transfer, not necessary execution products.

## Correct cold construction

Cold world creation starts from the actual published primary selection, the
captured source domain and exact profile. Visible binding rows may reference
their independently issued historical support. Keep those references separate
from current compiler namespace selection and resolve their demand through the
service. Capture the inputs from one publication owner; private construction
must not accept arbitrary independently obtained parts as coherent evidence.

Do not use `checked_value_context` over all retained values as the new open-world
constructor, even on a cold path. Its broad support merge can promote historical
custody into current implementation selection and reproduce the frozen Persona
conflict. Likewise, do not replace it with a renamed fold or precompute the same
merged graph on every publication. Cold capture may visit the primary snapshot
once; repeated small changes must use issued changes and shared references.

Rust retains genuine execution/provenance checks: equal interfaces are not equal
implementations; original dependency edges cannot be rebound; genuinely competing
required implementations still refuse. A public-module question should not solve
unrelated executable closures solely because the actor retains historical values.
Checking a cell that actually uses those values must validate the relevant exact
dependencies. The demand boundary must distinguish these cases structurally.

Existing exact-scope encoding may be reused as an encoding component or a
deliberately named legacy restore adapter if it preserves these distinctions.
That does not authorize reusing its old whole-context producer. Backcompat may
be broken: explicit version refusal is preferable to silent reinterpretation.

## Successors and lifecycle

Carry actual runtime changes to the service through existing publication
producers. Cover declaration, bind, import, alias, shadow, removal, source change,
fork, reload and restore. A single actor-wide revision counter is insufficient
if binding changes can occur without changing it.

Service preparation issues private successor facts from certified output and the
actual change. It must not infer a successor from the worker's completed-module
cache, which can exclude the just-compiled target. For multi-item cells preserve
the existing per-item visibility/publication rules: a prepared later item is not
yet a published binding. Partial effects do not become rollbackable by calling
the whole response a candidate.

Rust publishes the matching world and bindings only after the required output,
execution and END evidence. Reject stale candidates against their original base.
Retain exact restoration facts before durable publication, without serializing
the entire ancestor graph for each small update. No second transaction manager,
daemon-side mutable actor-current registry or ever-growing replay chain.

Forks can share immutable worlds and diverge independently. Changed reload uses
the agreed generation fence/drain contract; independent actors keep their worlds.
After eviction/restart, restore exact captured sources, including meaningful
absence, and exact historical edges. Do not reread live disk as the old world.
Accepted/uncertain source work cannot be retried by selecting a cold path.

## Parallel assignments and sequence

Keep the existing supervision tree. Each area uses its reserved workers and
independent reviewers; reassign a completed leaf instead of creating another
competing owner. Shared signatures have one owner and early paired compilation.

| Area | Execute now | Dependent next wave |
| --- | --- | --- |
| Host inputs | Actual published-session client/world ownership; lean cold construction; source capture; actor lookup entry before old preparation | Ordinary cell preparation and primary mutation producers; atomic successor association; delete redundant host compiler reconstruction |
| Warm workers | Sole world client/wire owner and Haskell source resolver; inspect operation; private realization and exact restoration | Prepare-cell/successor protocol and actual compiler implementation; retain necessary normal-module checking, remove duplicated setup where proven |
| Owned inputs | Immutable source/package backing and actual service lifetime; share exact row references without namespace promotion | Restore-product backing and bounded release across successors/forks; no repeated baseline transport or whole-history copying |
| Reload | Canonical source admission, clean END ownership, generation fence | Apply the same admission to world queries/cell preparation; successor adoption, scoped drain and recovery using the common client |
| Qualification | Independent cold-constructor/selection counterexample and production-path instrumentation | Joined lookup + bind + later-use + fork + eviction histories; exact frozen Persona and paired new-cell timing; deletion audit |

The host and warm supervisors must agree both the inspect and prepare-cell
contracts now. Implement the query slice first, while independent lifecycle,
mutation wiring, restore backing and test work proceed. A query milestone does
not postpone cell-contract design until after everyone has stood down.

After each joined milestone: compile affected production and test consumers,
exercise a small real actor history, independently review the cumulative change,
and fold upward. Campaign owns ordinary design arbitration and repairs. Escalate
only a genuine semantic strategy conflict; keep independent work progressing.

## Deletion and evidence gates

Maintain a callsite disposition in the engine work map: migrated through the
world client, intentionally distinct runtime consumer, explicit unsupported
profile path, or obsolete/deleted. Cover native lookup, notebook lookup,
ordinary cells, declaration-only preparation, renderer specialization, startup,
fork, reload and recovery. Not every product requires identical demands, but
no supported production caller may bypass the common service/capability owner.

The following cannot remain on the warm supported route:

- Full `actor_compile_view`/provenance construction merely to acquire a world.
- Repeated `capture_value_interfaces` and `checked_value_context` folds of all
  bindings just to ask a bounded question or prepare one changed cell.
- Compiler-only Rust closure reconstruction or whole-world hashing/encoding
  moved into publication, restoration-product issuance, or a cache initializer.
- Parallel legacy APIs or test-only constructors recreating authority that the
  new client is meant to own.

Keep genuinely needed runtime/native certificates and authority checks. Delete
or narrow mixed structures only after tracing their actual consumers. Explicit
unsupported CPP/profile cases remain observable and tested; they cannot silently
stand in for a successful supported-world hit or broaden on an uncertain retry.

Acceptance requires all of:

1. Real actor front doors use the published world client for inspection and
   ordinary cell preparation; test helpers do not supply handwritten worlds.
2. A fixed query and a small appended cell over growing unrelated history show
   eliminated baseline host visits, hashing, encoding, FD enumeration and
   duplicate host/full-input reconstruction. Track publication cost too. Measure
   GHC import/HPT activation separately: the ownership migration does not prove
   fresh-module checking O(delta). Actual new dependencies may legitimately
   increase work. Missing observations remain unknown.
3. Both kinds of historical dependency are covered: unused old custody does not
   become a current root; actually demanded issued dependencies retain identity
   and genuine conflicts still refuse, including catalog-backed Persona.
4. Same-tip mutations, stale successor commit, failed preparation, cancellation,
   uncertain END, fork divergence, eviction and cold restore preserve publication
   and authority. No tool/source-effect replay or accidental visibility.
5. The map lists old entry points removed and justified remaining consumers.
   Query timing and ordinary-cell timing are reported separately; the original
   expensive notebook frontend remains an explicit target until measured.
6. Exact joined production/test compilation and counted histories pass before
   final frozen acceptance. An initial query improvement alone is partial delivery.

The separate checked-import-context experiment remains evidence-driven. A
negative result there is not permission to abandon this Rust/service ownership
migration, nor proof that all ordinary GHC frontend work can be removed.
