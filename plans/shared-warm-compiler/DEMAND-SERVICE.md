# Compiler-owned worlds and demand-based operations

Status: accepted root architectural direction, 2026-10-19, following bounded
host, worker and independent HLS review. Concrete paired API signatures remain
with the campaign before their dependent implementation. This amendment controls
the ownership and wave sequence where earlier assignments describe a host-built compiler world. It
does not weaken the exact-selection, publication, or cleanup contracts.

Reviewed production source: campaign `a5fd3afffd`, host `c6c7d8ad5d`, and the
worker carrier review at `0bd6538e1e`. The evidence is in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`. Recheck the actual
integrated source before editing; these are investigation pins, not build bases.

## Decision

The Haskell compiler service owns long-lived compiler worlds and answers
specific questions about them. Rust owns notebook state, runtime execution,
durable artifact references, resource admission, and publication. Rust selects
an exact world and supplies actual changes; it does not reconstruct GHC's
derived world before every operation.

This changes more than transport. Caching a fully prepared Rust request after
building it is not delivery of this amendment. Moving the same whole-history
preparation into Haskell on every request is not delivery either.

GHC sessions already survive requests: `withResidentCompilerScopes` creates
`CompilerUniverse`, including its package environment and completed versions.
The repeated work remains around that session. Runtime admission captures all
reachable value interfaces; toolchain constructs complete request metadata and
physical inputs; `residentCompileOne` selects original nodes and reconstructs
an HPT for its attempt. Preserve the real ownership constraints while replacing
these repeated derivations with maintained compiler worlds and demand-driven
operations.

## Owners and facts

| Fact or mechanism | Owner | Cross-boundary representation |
| --- | --- | --- |
| Visible notebook bindings, lexical scope, explicit source revision and authored selection changes | Rust runtime publication owner | Immutable selection references and actual owner-issued changes |
| Current actor generation, execution grants, effects, cancellation and resource retirement | Existing Rust runtime/actor owners | Fresh operation authority, never inferred from a world handle |
| Exact prepared artifacts and issued native dependency bindings needed by the evaluator | Existing artifact/toolchain/runtime owners | Retained immutable products, exact references and certified compiler output |
| Compiler world: selected source/type environment, import resolution, HPT, decoded interfaces and compiler dependency validity | Haskell compiler service | Opaque world reference, portable restoration product, and typed query results |
| Package source lifetime and configured compiler profile | Existing admitted deployment/process owners, consumed at compiler boot | One shared retained source association; GHC establishes actual consumption |
| GHC object residency, NameCache, mutable checking attempt, interpreter epoch | Haskell worker and existing process owner | Worker-bound realization reference, request hold and settled outcome |
| Durable notebook commit and restart selection | Existing Rust recovery/publication owner | Published world restoration reference paired with bindings/source references |

The runtime's explicit choice of body A rather than B is an input fact. GHC's
calculation of which imports and interfaces an operation requires is derived
compiler knowledge. Keep that distinction. Rust still verifies the execution
products it consumes; removing duplicate compiler preparation is not removing
the artifact certification boundary.

The durable restoration product contains immutable source selections, certified
interface/artifact references and necessary configuration. It does not contain
serialized HscEnv, a log of effects to replay, or a host-maintained copy of the
compiler's mutable caches. Rust may retain it opaquely and store shared roots
incrementally. Full encoding or revalidation on every small commit is forbidden.

Some current Rust structures mix both roles. `RequestCompilerInputs` metadata
and native selections also support output certification and retained runtime
generations. Replace their compiler-only preparation with demand-specific
returned evidence; do not delete the exact runtime facts merely because the
same structure was also sent to GHC.

For example, `OriginalInstanceEnvironment` is a Complete/Missing/Unknown proof
marker consumed by activation and native origin checks, not a Rust copy of a
GHC instance dictionary. `ContextObservation` also supports semantic currentness,
checked host-binding prototypes and recovery. A worker-local reference cannot
replace those portable runtime proofs. Migrate their issuing/consuming contracts
explicitly and preserve exact identity and refusal behavior.

## Service interface

These are conceptual operations to map onto the existing owners, not a new
generic RPC framework or mandatory public names:

1. **Open or restore a world.** Authenticate an immutable source/selection
   snapshot under the admitted compiler profile. A genuinely cold acquisition
   can inspect the full selected input set. Retained alternatives do not become
   selected roots. The service returns a world reference and restoration facts.
2. **Prepare a successor.** Apply actual publication-owner changes against an
   exact base world. A change includes its issuing identity and actual new
   bytes/references, not an arbitrary caller patch. The result is private until
   runtime publication. Unchanged compiler facts are shared; compiler-owned
   dependencies determine invalidation. A missing base requires exact restoration.
3. **Ask a bounded question.** Inspection, cell checking, executable preparation,
   and explicit whole-world validation have distinct result contracts. Use the
   existing request types and pipeline selections where they express this.
   Lookup must not ask for executable products merely to learn a type. Checking
   must not publish bindings. Executable preparation returns only the actual
   output and required dependency evidence, with independently retained original
   edges preserved. Some questions legitimately need a large transitive closure.
4. **Publish through the runtime.** Compilation returns a result bound to its
   base world, private successor, operation and exact output. After the existing
   END and certification gates, Rust executes admitted runtime work once and
   publishes the bindings and matching successor reference atomically. Preserve
   the current whole-cell validation and ordered item/prefix semantics. A failed
   or stale commit cannot make the private successor the actor's active world.

Retaining a valid private compiler result after a failed runtime publication is
allowed acceleration. It is neither visibility nor authority. No distributed
commit is needed merely to keep an unpublished immutable compiler world alive:
the Rust publication owner decides which world is current. Its durable record
must contain enough exact restoration evidence before it commits that selection.

The current `residentCompileOne` deliberately excludes the target module from
its reusable completed-source inventory and strips target HPT/recovery entries.
Therefore the existing `CompilerUniverse` after a compile is not automatically
the successor notebook world. The compiler result issuer must prepare that
successor from its certified target/products and the actual publication delta.
For a multi-item cell, returned prefix references must bind exact item ordinals,
produced type/artifact identities and predecessor worlds. A prefix cannot expose
later values merely because all their type interfaces were compiled together.
Do not reconstruct this transition from broad retained output inventories.

## First query and visibility

The first query slice has a concrete opportunity: public-module
`StructuredInfo`/`StructuredType` currently pays for a checked query target even
though its result path uses module exports and name information rather than
that target's `TcGblEnv`. This is not permission to reuse `findModule` unchecked.
The existing helper has not established selected-world visibility by itself.

An explicit module question may inspect an unimported module available in the
world's captured source search domain or fixed package profile. Resolve its
exact unit/source/version there, not through arbitrary HPT presence or live
working-directory files. Retained implementation-only modules and generated
notebook internals do not become public merely because their code is loaded.
An uncached module may need its own necessary checking; remove the unnecessary
query target, not the module's actual prerequisites. Current lexical-scope,
availability/solver and arbitrary expression-type questions have different
requirements and retain their checking until separately justified.

Controls must cover an unimported public module, explicit source-versus-package
selection, hidden generated names and an old body retained only as a native
dependency. The exact source lookup receipt remains tied to the world.

The captured source domain must survive cold restoration, including modules
first demanded after restart and meaningful absence. An include-directory path
is not a source snapshot. Test a previously absent module created later on disk
and source A replaced by B while world A remains retained. Loading a module for
a question may enrich realization data; it does not implicitly add notebook
bindings, change source precedence or advance the primary world selection.

## Identity, sharing and recovery

Separate three identities in the existing typed owners:

- The logical immutable world, defined by selected source/definition/binding
  facts and compiler profile. It is independent of one query or physical worker.
- Its optional realization under an actual worker, NameCache and package source.
  Worker replacement invalidates that reference even if a pool slot is reused.
- A fresh operation, with demand, output reservation, native permission,
  cancellation, diagnostics and END settlement.

Do not key world identity by full request bytes containing the current target,
purpose or generated scaffold. Do not key it by actor name, canonical interface
equality, or a broad retained inventory. Exact implementation selection and
issued historical dependency edges survive the split.

Use the existing bounded shared worker pool. A world is not a dedicated process
or a transaction held while an actor is idle. Compatible immutable facts can be
shared; divergent worlds remain separate. Each worker keeps one active mutable
GHC attempt. Do not copy a raw HscEnv or reuse lazy details across incompatible
worlds merely because names compare equal. Interpreter residency has its own
epoch and existing retirement rules.

A missing realization before source acceptance permits restoration of the same
world in the same admitted protocol flow. A lost worker after source acceptance
remains uncertain and is not automatically retried. Restoring type/source state
never reexecutes notebook effects. Existing compile-time execution eligibility
rules still apply: TH/plugins or other untracked execution cannot be made
replayable by calling a product a snapshot.

Forking retains immutable world/source references and separately issued runtime
authority. Reload prepares privately, fences the actual old generation, drains
the authorized dependent resources, and publishes the new notebook/world.
Independent LLM worlds remain independent. The already compiled self-reload
continuation does not gain authority for new old-generation work.

## Persona-facing contract

Use the existing structured lookup/inspection surface as the frontend. The
resident interpreter supplies the actor's current published world automatically;
the model should not manage worker IDs, reconstruction manifests or a compiler
session merely to inspect a binding. Explicit retained evidence remains tied to
its own source/view, and a stale view must not silently become the current one.

Expose types, constructors, constraints and defining source as structured
results, preserving bounded presentation and retained detail. Inspecting the
type of a bound value does not evaluate that value. An arbitrary new expression
still needs the appropriate checker. Already retained owner-issued type evidence
may answer a matching inspection without another compiler invocation; do not
turn the service abstraction into a rule to repeat work whose answer is known.
No new model-facing tool vocabulary or GHCi REPL subsystem is required by this
wave. New convenience API work follows actual demonstrated friction.

## What changes in the current campaign

| Existing work | Disposition |
| --- | --- |
| Shared immutable interface rows and exact artifact ownership | Reuse. They back cold acquisition, changed inputs, output certification and recovery. |
| Persistent runtime declaration/value publication and exact identity fixes | Reuse, but retain primary runtime facts and provider references rather than a second compiler-derived graph. |
| Host incremental compiler-closure indexes | Audit each real consumer. Keep runtime execution/provenance indexes; move compiler-only invalidation and selection to the service. Do not build both implementations. |
| Source-free Ready/NeedsAcquisition/Rejected framing and atomic input retention | Reuse inside the service client. Ready must bind a real held world. It cannot require a complete host export before the hit branch. |
| Existing native process pool, BEGIN/END, cancellation and quarantine | Reuse. No second scheduler, process supervisor, or replay loop. |
| Generation admission and selective reload retirement | Continue. This governs runtime authority independently of optional compiler residency. |
| Package source bridge | Review minimal deployment/process-owned retention. Acquire once per actual service/deployment lifetime, not per query. Do not introduce permanent per-client root accumulation. |
| Checked import-context experiment | Still conditional. Owning a world does not itself prove raw typechecker environments safely reusable. Measure the remaining frontend floor first. |

The [required Rust client migration](RUST-CLIENT-MIGRATION.md) controls actor
integration, cold issuance and deletion. Existing exact-scope encoding components
may serve exact cold restoration, but their old whole-context producer is not
implicitly reusable: even cold construction must separate published primary
selection from historical binding custody. Ordinary requests and small
publications use the scoped world client and actual changes.

## Package source decision

The current compiler daemon can outlive a connecting host. Use one independently
retained immutable package source set per actual service epoch, acquired before
worker boot through the existing qualification retention provider. Connecting
clients and worker-slot rotations create no additional root sets. Share the
source association across that service's workers and their interpreters.

Attach normal release to the existing complete service/process retirement
outcome; retain the set and receipt on uncertainty. Host exit, END, socket
unlink or one child exit do not establish complete retirement. Do not add a
root registry, root service, distributed reference counter or normally permanent
per-client retention. If an existing deployment owner is shown to already own
both the roots and the service's complete retirement, transfer that obligation
instead of creating a redundant set; record that constructor before substitution.

Keep the initial supported package profile fixed. Validate actual expanded
package DB/unit paths and configured loader inputs at boot, retain the immutable
Nix closure, and preserve current receiving package/home selection checks.
Ambient loader overrides must not silently widen that profile. Wider plugin,
TH or arbitrary foreign-library configurations require their existing evidence
or explicit observation/refusal behavior; a generic native-loader auditing
system is not a baseline prerequisite. This protects supported deployment
lifetime and freshness, not arbitrary privileged mutation of immutable stores.

This follows the independently reviewed `PACKAGE-SOURCE-SERVICE-OPTION.md`
under the campaign evidence directory. The source owner and warm-worker owner
must agree the actual boot/retirement constructors before implementation.

## Revised work sequence and supervision

Keep the current supervision tree and accepted branches. Campaign owns the
shared contract; one implementation owner edits each cross-language protocol.
Continue independent repairs, compilation, reviews and merges while this
interface is settled. Do not restart completed work.

**Wave A — paired owner design.** Host and warm-worker supervisors each trace one
real lookup and one real cell. Jointly classify every currently supplied field:
runtime selection, compiler-derived input, executable provenance, physical
ownership, or operation authority. Identify the existing constructor that can
issue a world reference and its restoration evidence. Return exact caller maps,
deletion candidates and counterexamples; settle the shared API before coding.
The package owner supplies the minimal boot-source/lifetime contract in parallel.

**Wave B — first complete production slice.** Implement world open/restore and
lookup/check against it through the actual runtime front door. Borrow the
published reference before metadata/FD export. Derive required compiler facts
inside the service, reuse unchanged facts, and preserve current result
presentation. Prove a hit, cold restore, stale world refusal and independent
divergent worlds. Do not call unused protocol scaffolding a completed slice.

**Wave C — cells and publication.** Extend the same interface to whole-cell
checking, required executable products and exact ordered publication. Migrate
append, value-only publication, aliases, shadow/removal, source adoption,
captured forks and recovery. Bind original output, runtime values and successor
world without reconstructing a complete map. Retain an explicit checked cold
fallback only for named unsupported transitions, measure it, and remove it as
each transition migrates. Those routes remain incomplete until migrated.

**Wave D — remove the old route and qualify.** Delete host compiler-only
derivations and hot full-input exports made obsolete by the actual callers.
Remove bypass/test-only constructors. Exercise reload retirement with the new
world handle, eviction/restart and joined histories. Update the compiler map,
then measure the exact integrated frozen bundle against baseline. Decide the
separate import-context experiment from attributed remaining costs.

The wave names describe dependent contracts, not a global serial barrier. Row
ownership, truthful diagnostics, reload ownership and qualification can advance
independently. Each merged slice must leave a working production path; an
unsupported new option must refuse rather than silently succeed.

## Acceptance and falsifiers

- Fixed small query/cell work against growing unrelated retained history must
  not cause growing host scope construction, image encoding, FD enumeration or
  worker full-world hydration. Count each stage separately; zero wire bytes
  does not prove zero host preparation.
- New or changed facts invalidate the affected compiler results, including
  package-versus-home absence, instance/lexical changes and equal-interface
  different-body selections. Independent expected selections must not reuse
  the production filter as their oracle.
- A and B may coexist in retained storage; selecting B excludes unused A from
  compilation roots. An actually issued dependency on A still refers to A and
  genuine conflicts still refuse. A runtime native offer is not the same as an
  operation's selected executable groups.
- A private successor cannot enter current notebook state without its matching
  binding/source commit. Test stale writers, failed execution/publication and
  cancellation using public production owners.
- Two actors/worlds can alternate and diverge without cross-contamination. Drop
  one while the other retains a needed lazy interface or executable dependency.
- A worker eviction loses acceleration, not durable facts. Restore the same
  world, verify an actual target, and prove no effect replay. Accepted-uncertain
  operations and unconfirmed cleanup retain their existing obligations.
- No query promotes current runtime authority, bypasses capability checks or
  turns a cache hit into an executable grant. Restore does not revive retired
  source-generation authority.
- Source-backed package reuse must consume the retained immutable deployment.
  Mutable/uncovered inputs keep explicit observation/refusal semantics. No
  retroactive blessing of an already warmed unknown package environment.
- Repeated small commits and optional retention must remain bounded in memory
  and storage. Count unique bytes, pinned backing bytes and reachable strong
  owners, including GHC lazy closures and interpreter state.

Measure startup separately from repeated queries and cells, with host preparation,
transport, world acquisition, import/typecheck, codegen, certification, execution
and publication attributed. A tenfold gain remains a target, not a consequence
we can claim from this design. Typechecking new code and emitting demanded
products remain real work.
