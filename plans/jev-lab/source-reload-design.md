# Source reload — design

Answers `plans/jev-lab/source-reload-spec.md`. Written before the
implementation; file:line citations are to the tree at the time of writing.

## The shape in one paragraph

A Shoal run already captures its Haskell source roots once, into
`<run_root>/workspace/sources/<capture>/<index>`, and hands those directories
to every compile as `--include` roots. Reload adds a second, *mutable* layer
in front of that frozen floor: `<run_root>/workspace/revisions/<revision>/<index>`,
reached through one symlink `<run_root>/workspace/active`. The include list
every compile receives names `active/<index>` **before** the frozen
`sources/<capture>/<index>`, so a module present in the active revision
shadows the frozen copy. Publishing a revision is one `rename(2)` of that
symlink. Nothing else in the compile pipeline changes: the include vector's
length and entries are fixed for the life of the run, and only the bytes
behind one path move.

## Where a reloaded revision's compiled source lives

`<run_root>/workspace/revisions/<revision-id>/`, with one subdirectory per
configured source root (`0`, `1`, …, in the frozen config's root order) plus a
`resources/` directory holding the generated `Shoal/Source/Revision.hs`.
`<run_root>/workspace/active` is a symlink to the currently published
revision directory.

`run_root` is `~/.cache/tidepool/shoal/runs/<run_id>`
(`tidepool/src/shoal.rs:468-472`), outside the workspace, so a revision tree
can never be re-captured into itself.

Revision `0` is materialized at driver-compile time from the frozen capture
directories, so `active` always resolves and the include list is well-formed
before any reload happens. It is byte-identical to what the run froze.

## How the frozen workspace's tamper check is respected

`FrozenWorkspace::load` re-reads every path in `frozen.files` and compares a
blake3 digest against the manifest, refusing the run with "frozen workspace
input changed" on any mismatch (`tidepool/src/shoal/workspace.rs:75-82`).
`frozen.files` is keyed by paths under `sources/<capture>/…` and
`resources/Shoal/Workspace.hs` (`workspace.rs:104-107`, `199-202`).

A reload therefore **never writes inside `sources/<capture>/` or the frozen
`resources/`**. It writes only under `revisions/` and moves the `active`
symlink, both of which are absent from `frozen.files` and so invisible to the
tamper check. The frozen capture keeps its role: the run-stable, verified
record of what the run started from, and the floor the active layer shadows.

This is also why reload does not mutate the frozen copy in place. Doing so
would make the run's own restart-safety check (`load` is called again on a
warm `run_root`, `workspace.rs:65-84`) fail, turning every successful reload
into a corrupted run.

## How the reverse-dependency closure is computed and rebuilt

It is not hand-rolled. GHC already owns the module graph, and
`validate_workspace_program` already compiles the whole Shoal driver against a
`FrozenWorkspace`'s include roots (`tidepool/src/actor_host.rs:1773-1840`),
importing `Shoal.Workspace` plus every configured module
(`workspace.rs:231-233`, used at `actor_host.rs:1806-1808`).

The reload check is that same compile with `active/<index>` replaced by the
*pending* revision's directories. If `Project.CommandEvidence` changed, GHC
recompiles it and everything in the driver's import closure that depends on
it, including `Project.RunAhead`. A type error anywhere in that closure fails
the whole compile, and nothing is published.

**Scope, stated plainly.** The closure is everything reachable from the
configured module list (`[haskell] modules`) plus the driver. A module that
only an ad-hoc cell import reaches is not in that set; a break in it surfaces
at the cell that imports it, not at the reload. The reload verb therefore
takes a `[Text]` of additional modules to pull into the checked closure, so a
program can widen the transaction to cover what it actually uses. This is the
inverse of the spec's optional "targeted single-module option": widening is
safe, narrowing is what produces surprising partial activation, so only
widening is offered.

## Revision identity

Content-based, computed with the mechanism that already keys a compiled
artifact. `tidepool-toolchain::cache` fingerprints an include root by content,
keyed by each file's path relative to that root, over `.hs`/`.lhs`/`.hs-boot`/
`.lhs-boot` files, globally sorted so traversal order does not reach the digest
(`dependency_source_manifest`, `tidepool-toolchain/src/cache.rs:136-142`;
`fingerprint_dir_relative`, `cache.rs:547-572`; framed in original root order
at `cache.rs:515-521`).

That function is private today. This change exports it as
`tidepool_toolchain::cache::source_root_manifest` (path → hex digest) and
`source_roots_identity` (one blake3 over the ordered manifests). The revision
id is `source_roots_identity` over the captured root directories, framed with
the frozen workspace's own `identity` (`workspace.rs:179-186`) so a revision
is only ever compared within one run's config, prompts and library build.

No new identifier issuer is added. The per-revision generation number is a
display-only counter recorded in the revision's `revision.json`; it is never
identity, exactly as the spec requires.

**Circularity.** `Shoal/Source/Revision.hs` is generated into the revision's
`resources/` directory *after* the id is computed, and carries only the id —
the same ordering `freeze` uses for `Shoal/Workspace.hs`
(`workspace.rs:179-203`). Two captures with identical source therefore produce
the same id and the same generated module.

## Atomic publication

1. Resolve the source roots from the frozen config: `[haskell] source_roots`
   canonicalized under `.shoal`, then `flake_source_roots`
   (`workspace.rs:300-402`). Re-resolving through `nix flake archive` is what
   keeps pinned inputs immutable and makes `[haskell.flake_overrides]` the
   working-change route the spec names; when `flake_sources` is empty the
   function returns without touching `nix` (`workspace.rs:309-311`).
2. Capture each root into `revisions/.pending-<uuid>/<index>` with the existing
   `capture_sources` walk (`workspace.rs:415-421`), which already refuses
   symlinks and skips runtime/build trees.
3. Compute the revision id from the captured tree. If it equals the active
   revision's id, remove the pending directory and answer `ReloadUnchanged`.
4. Write `resources/Shoal/Source/Revision.hs`, then rename the pending
   directory to `revisions/<id>` (an existing directory with that id is
   byte-identical, so the pending copy is simply discarded).
5. Compile the driver with `active/<index>` substituted by
   `revisions/<id>/<index>`. On failure, answer `ReloadRejected` carrying the
   rejected revision and the rendered diagnostics. The `active` symlink has not
   moved, the previously compiled graph is still installed, and nothing under
   the workspace was written — the edited files are exactly as the model left
   them.
6. On success, `rename(2)` a fresh symlink over `active`. That is the
   publication, and it is atomic: a concurrent compile opening `active/<index>`
   sees either the whole old revision or the whole new one.

Reloads are serialized by a mutex in the reload service, so two actors cannot
interleave step 5 and step 6.

## How a later cell picks up the new revision, and why the requesting cell does not

An actor's per-cell compile builds its include list once per cell from
`ActorWorkbenchSource::base_include` (`tidepool-actor/src/resident_workbench.rs:211`,
read at `:236` via `SessionCompileView::include_paths`,
`tidepool-runtime/src/session/view.rs:298-304`). That vector is fixed at actor
construction — there is no setter and none is added here. What changes is what
`active/<index>` resolves to on the filesystem when GHC opens it.

- The cell that called reload was compiled, and its machine code installed,
  before the symlink moved. Its remaining computation runs that code.
- The next cell's compile resolves `active` afresh and reads the new revision.
- The compiled-artifact cache cannot serve a stale hit: `invocation_key`
  content-fingerprints every include root (`cache.rs:466`, `:515-521`), and
  `fingerprint_dir_relative` walks through the symlink, so a new revision is a
  different key.

The same substitution reaches all three consumers of the run's include vector,
because all three receive the same vector built in `compile_driver`: the
session library's declaration-validation include
(`actor_host.rs:1878-1879`), the resident machine
(`actor_host.rs:1893-1904`), and the per-cell workbench source
(`actor_host.rs:1946`). No per-crate API churn is needed, which is the main
reason the symlink layer was chosen over making `base_include` swappable.

**Bindings keep their implementation.** A value already bound lives on the
resident heap as a session `Val` generation; reload does not touch the heap, so
`oldHelper` keeps the code it was built from. **Declarations do not**: a
session-lib declaration is source in the session root, and GHC will rebuild it
against the new revision when its dependencies changed. That is requirement 3's
rebuild rule applied to session-local modules, and it is the honest behaviour —
but it is a real difference from bindings and is called out in the receipt's
documentation rather than hidden.

## Provenance, and where it comes from

Ordinary data, from two places.

- **Runtime.** `sourceStatus` answers a `SourceStatus` carrying the active
  revision and the latest observed disk revision, each a `SourceRevision` with
  its identity, its display generation, and the per-module `(name, digest)`
  list read from the revision's own manifest. That is the spec's "currently
  active snapshot" and "latest observed disk snapshot", and
  `Source.activeRevision "Project.RunAhead"` is a lookup in
  `revisionModules` rather than a separate verb.
- **Compile time.** Each revision directory carries a generated
  `Shoal.Source.Revision` module exporting `compiledSourceRevision :: Text`.
  An authored workspace module that records it gets, honestly, the source
  snapshot *it* was compiled against — the spec's own stated honesty limit
  ("compiled against this source snapshot is a useful, honest starting
  point"). It is importable, not auto-imported into every cell preamble:
  auto-importing would put the module on the required-import path of
  `validate_workspace_program` and the recipe-check compile
  (`tidepool/src/actor_host/recipe_checks.rs:186-202`), which do not carry the
  active layer.

`Shoal.Source` joins `Shoal.Workspace` as a reserved module prefix
(`workspace.rs:189-193`).

## Surface

One effect, `Source`, defined in `tidepool-protocol` (the live path for
anything an actor calls; `tidepool-mcp/src/effect_defs.rs` holds only
unmigrated base-eval effects). It is `dispatched: true`, so it is serviced
synchronously by an `EffectHandler` in the bootstrap handler list
(`actor_host.rs:1893-1900`) rather than suspending to the actor kernel.

```haskell
reloadSource :: [Text] -> M (Either SourceError ReloadOutcome)
sourceStatus ::           M (Either SourceError SourceStatus)

data ReloadOutcome
  = ReloadUnchanged SourceRevision
  | ReloadPublished SourceRevision SourceRevision [Text]
  | ReloadRejected  SourceRevision SourceRevision Text
```

`ReloadRejected` is a value, not an error: a failed typecheck is an expected
result a program can handle. `SourceError` is reserved for the case where
there is no workspace to reload at all. The handler holds a trait object
implemented in the `tidepool` composition root, because the typecheck step is
`compile_driver`, which lives there.

A convenience tool is not part of this change; the Haskell operation is the
only route, which the spec permits.

## Compiled tool records

A workspace may name a record of Haskell-backed tools with
`[haskell] tools = "Project.Tools.tools"` (`workspace.rs:24`, `:52`, `:112`,
`:210`; selected into the workbench at
`tidepool-actor/src/resident_workbench.rs:289-299` and
`actor_host.rs:1956-1963`).

**What the record actually is.** `ResidentActorWorkbench::prepare_tools`
(`tidepool-actor/src/resident_workbench.rs:1850-1968`) compiles one fragment,
`_ <- Tidepool.Agent.Contract.installTools @(<ActorEffects>) <entry>`
(`:1873-1875`), reads the declared schemas out of the `AgentToolsInstallWith`
suspension it produces (`:1913-1924`), and retains the parked continuation as
an `Arc<RootCustody>` heap root (`:1925-1965`). It runs exactly once per
actor, latched behind `policy_installed`
(`tidepool-actor/src/resident_actor.rs:3593`), and the result is written once
into `compiled_tools` (`resident_actor.rs:589`, assigned at `:3601-3607`,
never reassigned). A call clones that `Arc` (`resident_actor.rs:4555-4581`)
and applies the retained value to the invocation data —
`begin_tool` is explicit that "No source compiler is involved"
(`resident_workbench.rs:1970-1987`).

Against the four properties asked for:

1. **Schema and handler come from the same revision.** True. They are two
   products of one compile of one module against one include list, and there
   is no path that advances either alone: the declarations are decoded from
   the same suspension whose continuation becomes the dispatch value.
2. **A call already accepted keeps its implementation.** True, and more
   strongly than for notebook bindings: the dispatch value is a retained heap
   root that reload never touches, and a call is an application of it.
3. **A failed reload leaves the previous tool record active.** True. The
   symlink does not move and nothing recompiles.
4. **The tool's source revision is inspectable.** Partially. The active
   revision is inspectable through `sourceStatus`, and the tool module can
   record `compiledSourceRevision` itself. There is no per-invocation record
   tying a completed tool call to the revision that served it.

**Does a reload refresh a running actor's tool record? No — untouched.**
`prepare_tools` is a one-shot at actor startup and there is no re-derivation
path anywhere (`resident_actor.rs:3466-3477` treats a second attachment as a
protocol error). A reload publishes a revision that every later *cell* compile
sees; an actor's already-installed tool record is not among them. The refresh
boundary is the actor's next incarnation, which now picks up a reloaded
revision without a new run. Building live tool hot-swap is deliberately out of
scope.

**Authority across a reload cannot widen.** An actor's effect row is exact
`ActorEffectKey` membership (`tidepool-actor/src/role.rs:61-80`), checked at
admission — `permits_child` / `respects_role_ceiling` (`role.rs:417-456`,
called at `resident_actor.rs:1641-1668`) — and then rendered once into
`ActorSessionContext.haskell_effects_alias` at descriptor construction
(`tidepool-actor/src/descriptor.rs:232-241`). `prepare_tools` splices that
baked alias into `HostedToolEffects` before compiling the tool record
(`resident_workbench.rs:1860-1867`). So a reloaded tool module that demanded a
wider row would fail to typecheck against the admitted alias at the next
incarnation rather than acquiring the capability — it fails closed. Within the
current incarnation nothing changes at all. Note that authority is an
admission-time property compiled into the dispatch closure, not a per-call
check; this design neither weakens nor strengthens that.

**Standing documentation.** `examples/shoal-workspace/.shoal/skills/shoal-command/references/hosted-tools.md`
says "Schemas and handler code freeze at actor startup; calls apply retained
compiled code to input data. Changes require a new package/run, not a live
file edit." The first sentence stays true. The second becomes imprecise: after
this change a source reload plus a new actor incarnation picks up an edited
tool module within the same run. That sentence is corrected as part of this
change.

## Not implemented in this cut, and why

- **"Identify the two revisions" in a type-mismatch diagnostic**
  (spec §Semantics 2). GHC prints both sides of a mismatch with the same
  module-qualified name; distinguishing them needs the diagnostic to carry
  each type's originating module fingerprint, which is extractor and
  GHC-diagnostic work. Deferred, per the brief's instruction to prefer a first
  cut that lands. The information a program needs to *explain* such a failure
  is available (`sourceStatus` names the active revision and every module's
  digest); only the compiler's own message is silent.
- **Live re-derivation of an actor's tool record.** Confirmed one-shot at
  actor startup; a reload does not refresh it, and the refresh boundary is the
  next actor incarnation. Recorded as follow-up rather than built, per the
  instruction not to expand scope into tool hot-swapping.
- **Deleting a module.** The frozen capture stays on the search path beneath
  the active layer, so removing a file from a source root stops it from being
  *updated* but does not stop it from being *importable*. Removing a module
  from the graph remains a new-run operation.
- **Recipe checks** (`shoal check --recipes`) still compile against the frozen
  capture only. They validate the authored package at a run boundary, which is
  a different question from what is live in this session.
- **Per-invocation provenance** ("the revision used by each recorded
  invocation, especially across `replace`"). The runtime does not record a
  revision per completed call today, and claiming one would break the spec's
  own honesty limit.
