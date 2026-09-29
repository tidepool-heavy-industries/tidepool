# Module product review handoff

This is the Git transfer packet for `foundation/module-product`. It is a
producer proof, not a durable module cache or a native demand compiler. Review
it against the current `foundation/integration` head before taking either
consumer across this boundary.

## Exact candidates and checks

- Base: main `a2e98636bac3358ea2796e75f6fb00b9c693c48e`, which includes the
  compiler's paired memo product. The branch then replays the four reviewed
  image-lifetime commits through `191cdfb1c`.
- `c7dac8efda7608f1aa61e39f5a52682300d4f090`: source-less GHC `.hi`
  rehydration gate. `6a98eab4694baa8f34fe9bd3304163011d4611ae`:
  product-only fat interfaces and entry-free, group-local projection.
- On `6a98eab`,
  `bash scripts/dev-shell.sh bash -lc 'cd bridge/haskell && cabal test prepared-stg-pipeline-test --test-options=--module-product-roundtrip'`
  executed one suite/one case and passed. The corresponding
  `--test-options=--retained-scope` command executed one suite/one case and
  passed. `bash scripts/dev-shell.sh bash -lc 'cd bridge/haskell && cabal build tidepool-extract-bin'`
  built the worker. `git diff --cached --check` passed before the commit.
- The roundtrip fixture wrote a 2,747-byte fat `.hi` and a 2,402-byte skinny
  `.hi` (+345 bytes, +14.36% for this fixture). It checked the HPT had no
  `mi_extra_decls`, raw `readIface` retained them after serialization, and a
  new GHC session typechecked an importer after the defining source was hidden.
  These bytes are one fixture measurement, not a general memory or throughput
  result. The source-less consumer reuses the producer's already discovered
  module graph and summary; it does not prove fresh `depanal` can discover an
  absent home source.
- A subsequent narrow fix in this handoff makes group projection reject a
  typed-site failure owned by the demanded group. Its new failure-path test
  and the Haskell target have **not** been compiled or executed since that fix;
  the shared compiler slot was not available. The same unverified patch adds
  explicit product mode and restores ordinary leaf interface elision after
  review found that `6a98eab` always paid tidy and interface construction.
  Rerun the exact roundtrip command above first. That gate now requests both
  ordinary and product modes and checks product mode includes a leaf interface.

## Producer contract now present

`bridge/haskell/src/Tidepool/GhcPipeline.hs` now has explicit
`PreparedProducts` selection. Only that mode puts each executable module's
exact fat `ModIface` in `PreparedPipelineResult.pprProductInterfaces`, keyed by
its home `ModuleName`. The interface and `PreparedModule` come from one tidy
result. Ordinary `PreparedStg` preserves the baseline early exit for a leaf
with no later home importer and builds a skinny interface only for a needed
importer. Product mode captures even leaf interfaces, while HPT installation
always strips `mi_extra_decls` and occurs only for a later importer. Memo
reuse in product mode requires a retained fat interface; an ordinary memo
entry that is skinny or elided is rebuilt. Ordinary reuse of a previous fat
memo entry still installs a skinny HPT interface and returns an empty product
map. The request-local result owns references to fat interfaces; there is no
durable writer or reader of them yet. The private `ProductInterface` sum type
still admits `InterfaceElided`, so a future writer must refuse an absent pair
rather than assume every path was covered.

`bridge/haskell/src/Tidepool/ExecutionProjection.hs` exports
`projectPreparedModuleGroups` and `projectPreparedModuleGroupsSelected`.
`ProjectedGroup` and `ProjectedGroupBody` live in `ExecutionSchema.hs`. Group
IDs are original prepared STG ordinals, assigned before retained-body and
demand filtering. `dropRetainedTops` filters complete binding-group elements
only when all their binders are retained; selected ordinals filter complete
groups too. A mixed retained/fresh recursive group remains one local group.
Projection starts one state per group: its own signatures, globals, constructors,
operations, expressions, types and sites are complete, while other top groups
in the same module become explicit globals under the complete module's
stable `SymbolIdentity` map. The result has no entry. Its fields are an
in-memory shape only: no neutral CBOR codec, Rust reader, validator, product
version or exact owner tag for a global exists yet. Implicit synthetic tops
can occur in more than one group-local body; a later batch assembler must
remap/deduplicate them by checked identity and content.

The compiler fingerprint fix is present unchanged in this branch:
`RetainedUnfoldings.hs` has the same Git blob as current integration
(`b409df6bfcacac4d1d8090f42578d2a38f8e734c`). `compileFront` scopes
`mfHscEnv` with `scopeRetainedHscEnv (ms_mod modSum)` before optimization;
the fat-interface option is added to that saved environment and does not
replace its module-scoped plugin option. The retained-scope test passed, but
the 0/1,000/10,000-symbol cost gate remains to run on the target host.

## Consumer boundaries and decisions

1. The current Rust `WireProgram` and `PreparedProgram` require one entry and
   one validated expression arena (`tidepool/repr/src/execution_schema.rs`,
   `codec.rs`, `validation.rs`). Codegen consumes the whole validated program.
   Add a closed neutral product codec and Rust validation, then seal selected
   groups into one checked batch with table and expression-ID remapping. Do
   not manufacture a dummy entry or slice a flattened `WireProgram`.
2. A same-module cross-group global has only `SymbolIdentity` and optional
   `required_generation` today. The durable import must name its exact source
   `ModuleVersion` or an exact retained binding/source instance. Current
   `link_program` resolves by symbol plus signature/representation/generation;
   this is insufficient for two source instances with one spelling.
3. `tidepool/toolchain/src/cache.rs::InvocationKey` is the existing recipe
   owner. `DependencyEvidence::valid` checks consumed source bytes, selected
   paths, negative resolution candidates and `cache_safe` before reuse.
   Extend the existing atomic bundle/manifest in `artifacts.rs`, retaining
   the full evidence rather than only its digest. Unknown compile-time effects
   and incomplete/changed negative witnesses must cause a miss. No durable
   module product file or cache consumer has been added by this branch.
4. The current runtime `PreparedEngine.code_exports` stores retained package
   top handles for the life of the session. Those handles keep installed
   programs and native code live, even when no demand uses an export. Demand
   compilation needs a weak metadata catalog for unused exports, with
   explicit semantic leases for already materialized CAF state. Native
   `ImageRegistry` itself is weak; machine installations, parcels, values,
   continuations and active compiles are the intended strong code owners.
5. Batch admission must close every statically reachable recursive group
   before entry, off machine checkout. Direct calls stay inside a group;
   cross-group calls import through the destination-local installation
   environment. Overlapping demands must share one group identity and one
   compiled batch where appropriate. No runtime JIT demand trap is planned.

## Review packet for the next host

- Validate the opt-in product mode, ordinary leaf elision and narrow typed-site
  rejection fix, then review exact symbol
  mapping for internal floated tops and the group-local synthetic declarations.
- Decide the typed `SourceModule(ModuleVersion)` versus retained binding import
  representation and the batch table remap before publishing any neutral
  bytes. A version digest alone must not replace the underlying dependency
  and negative-resolution evidence.
- Decide where the module product's raw `.hi` bytes, neutral body and exact
  evidence are paired atomically. Existing `InvocationKey` bundles can provide
  conservative whole-invocation reuse first; per-module reuse requires a
  separate validity proof from the worker's exact direct witnesses.
- Measure interface construction time and retained bytes at 0, 1,000 and
  10,000 retained symbols, plus unused-group native code and final-owner
  release. The small `.hi` size comparison above is only a functional gate.
- Integrate after current `foundation/integration` and compile the joined
  Haskell/Rust boundary. The branch itself has not been tested against that
  integration head.
