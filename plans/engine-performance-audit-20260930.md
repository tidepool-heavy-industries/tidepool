# Independent engine performance audit

Audit scope: checkpoint `7fb5c0061723583b4a9a9f0bd077a52aedb43aaa`
against main `cbcdbfc82e22488310c11b1ebd2a06b69c5c71ed` and the isolated runtime,
compiler, SOURCE boot and actor candidates available on 2026-09-30. Subsequent
repairs are not silently included in the original conclusions below. This was
a read-only source and retained-evidence audit; it did not run a provider,
restart any daemon, or execute a new performance battery.

There was **no demonstrated same-version before/after runtime regression** in
the available evidence. The following findings are concrete structural costs
or lifetime defects; their cost estimates are inferred until separately
measured. The preexisting codegen, heap, repr and inner effect-machine loops
were unchanged relative to the checkpoint.

## Ranked findings

1. **P1: failed actor abort acknowledgment could retain a self-cycle.** At actor
   candidate `da8927be0a`, `exomonad/actor/src/resident_workbench.rs:957-974`
   installed `Arc<ParkedHoleAbortState>` into its own `unconfirmed_owner` when
   holes remained uncertain. The captured abort closure also owned the session
   registry, actor source and runtime handle (`:1032-1040`). Failed machine
   admission only warned (`:1048-1060`); production had no handle to the owner
   after the registration disappeared. The existing test (`:15986-16021`)
   resurrected a saved weak pointer and injected retirement manually. An
   uncertain frame must retain resources, but the existing machine/realm
   cleanup owner should hold that uncertainty. Meaningful verification forces
   cleanup admission failure, drops all external registrations, proves
   unrelated realm cleanup insufficient, and proves owning-realm or confirmed
   machine-loss cleanup releases the resources. This was a lifetime defect,
   not a measured latency regression.

2. **P2: newly mandatory family validation paid avoidable quadratic work.**
   `bridge/haskell/src/Tidepool/FamilyConsistency.hs` collected the full HPT
   retained closure and fresh local instances after both frontend paths;
   `GhcPipeline.hs:1533` and `:2294` are the production callers. At audited main,
   `DeclarationJoin.hs:224-228` deduplicated exact coercion axioms with `nubBy`.
   Its full family/injectivity checks (`:316-342`) rebuilt the complete combined
   environment, excluding one axiom, for every injective instance. Across N
   distinct families this introduces N whole-environment rebuilds despite
   GHC's per-family lookup. Preserve full-closure conflict and injectivity
   validation; replace exact-identity dedup with a Unique set and share the
   combined injectivity environment. Measure immutable real GHC axioms at
   increasing family counts, including compatible, incompatible, package,
   associated, polymorphic injective and same-family cases.

3. **P2 integration risk: admitted empty context could defeat warm compiler
   reuse.** `tidepool/toolchain/src/artifacts.rs:268-276` converted a missing
   declaration context into an explicit empty exact context. Admitted selection
   (`:307-326`) therefore always selected exact mode. In
   `GhcPipeline.hs:1295-1305`, exact mode freshened mutable state and disabled
   the resident interface/Guts memo. Before this change, binding-only requests
   could use the warm path. Correct isolation requires fresh mutable state;
   immutable certified products should be reused independently. At audited
   main the admitted actor path did not yet have a production consumer: the
   actor still called `check_cell_with_fold`; admitted methods appeared only
   in runtime tests. Verify actual production routing before claiming a user
   regression. Compare identical warm requests with compiler phase timings,
   accepted/missed products, and allocation totals.

4. **P2/P3: completed-prefix publication repeatedly copied and reprocessed
   historical state.** Runtime `admission.rs:259-273` rebuilt all accumulated
   interface entries at each item settlement and converted historical byte
   slices into fresh Arcs. `:282-283` retained another full lexical scope lease
   per settlement; `:303-361` hashed the full view/interface/native inventory,
   including all interface bytes (`:334-337`). `turn.rs:2542-2550` and
   `:2994-2999` materialized every prefix interface again for whole/item
   compilation. N item settlements over B baseline bytes imply repeated NB
   work plus growing-prefix quadratic work. Publish immutable deltas, index
   module identity in the owning collection, share existing byte Arcs, and
   make each exact snapshot own its own lifetime lease. Compiler full-context
   proof must remain intact. Measure 1/10/100-item sequences with baseline
   interface bytes, cumulative bytes copied/hashed/written, active snapshot
   leases and final resource reclamation.

5. **P3: checked targets deep-cloned prepared program graphs.**
   `tidepool/toolchain/src/checked_cell.rs:552` wrapped `target.clone()` in a new
   Arc while `runtime/src/session/turn.rs:1787` retained the original target.
   `PreparedProgram` owns a cloned `WireProgram`
   (`repr/src/execution_schema.rs:997-1000`). Retained exact compiled items
   therefore owned additional graph copies; structural equality could compare
   the graph again. Share the original immutable Arc and prove target identity
   at the owning producer boundary. Measure allocated/retained bytes for large
   targets through actual admitted compilation and prefix retention.

6. **SOURCE product reuse conservatively loaded unrelated accepted modules.**
   `bridge/haskell/src/Tidepool/HomeProducts.hs:88-109` used `load'` over the
   full selected graph whenever any boot module was present, then typechecked
   boot interfaces explicitly. This can rebuild unrelated acyclic accepted
   members as well as the required boot SCC. The previous path missed SOURCE
   products entirely, so this is an incomplete optimization, not evidence of
   a slower overall request. Keep SCC knot/boot authority with GHC and reuse
   unrelated certified immutable products without unnecessary fresh loads.
   Measure mixed SOURCE-SCC/acyclic graphs with phase counts and exact reused
   products before changing this boundary.

## Evidence limits and dependency/build costs

The retained SOURCE evidence in
`/tmp/tidepool-wave-next-source-boot/target/completion-evidence/source-boot-fba969e3c/evidence.json`
recorded a real Haskell suite and Rust test, the latter 4.19 seconds. It
explicitly distinguished fresh GHC loading of boot SCCs from immutable
extraction/lowering reuse. Actor cancellation timings of 111.042/113.055
seconds included cold preparation. Browser compiler response timings of
83.201/98.335 seconds lacked comparable same-version baseline runs. None of
these establish a runtime regression or speedup.

Cargo dependency fanout was unchanged apart from the harness revision in the
examined delta. Haskell implementation remained in the shared internal library,
not recompiled independently into each test implementation. No new inner-loop
engine lock or scheduler serialization was found. Fixture compilation, cold
builds, executable linking, compiler-request work and mutable runtime execution
must be reported separately.

This report records the independent audit. Later bounded family-validation
measurements and repairs are retained separately beside it.
