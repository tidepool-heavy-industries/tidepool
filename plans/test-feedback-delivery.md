# Test feedback delivery

## Outcome and release boundary

Make ordinary native and browser changes cheap to verify while preserving
behavioral, recovery and provenance coverage. This work proceeds independently
of the reconciled coding/companion release. A cleanup candidate must compile
its affected production and test targets and execute focused controls before
integration; source review alone is not qualification.

## Parcels and acceptance

1. **Profile ownership.** Harness and its Tidepool consumer use
   `tidepool.profile`. Fast development uses O0 first-party Harness code and O2
   dependencies; debug uses O0; Harness production retains O2. Preserve
   Tidepool's existing production/hot-library policy. Remove unconditional
   optimization from compiler identity flags. Verify actual Buck compiler
   arguments for standalone and external-cell library, binary and test actions.
   The profile policy source participates in production source identity.
2. **Build inputs.** Separate production and test-only Rust/browser source
   sets. Keep all test inputs on test targets and a complete test typecheck
   target. Demonstrate that a test-only edit does not invalidate production
   library or browser distribution actions. Preserve authoritative schema,
   source identity and asset sealing producers.
3. **Pure Harness components.** Extract schema normalization, projection and
   validation into `harness-schema`, and pure presentation DTOs/validation into
   `harness-presentation`. Keep request preparation, finalization authority,
   durable leases and Store transactions in Harness. Preserve public API paths
   through reexports and explicit error conversion. Compile both leaf and
   integration consumers; retain cross-boundary properties in Harness.
4. **Pure Haskell component.** Extract ExecutionSchema and ExecutionEncode into
   a GHC-free Cabal library. ExecutionIR remains compiler-owned. Project Buck
   targets from Cabal metadata. Separate candidate manifest tests from pure
   encoding checks; obtain valid candidate bytes from the production issuer,
   then introduce explicit malformed variants. Execute the selected Tasty
   suites and compile the production worker.
5. **Fixture and assertion cleanup.** Construct positive histories through
   production owners. Consolidate repeated assertions and remove vacuous,
   obsolete or redundant tests, preserving their distinct obligations at the
   cheapest useful boundary. Remove production helpers left with no consumer.
   Keep deliberate storage corruption and crash/recovery tests. Use Node for
   pure browser helpers and browser environments where behavior requires them.

## Review and measurement

Each parcel records its exact revision, changed targets, executed counts,
retained failure controls and any obligation moved or removed. Review authority
boundaries and dependency closure before integration. Coordinate shared Cargo
and Buck edits and actual checkout/output ownership, not all build commands.

Compare no-op builds, test-only edits, pure-component edits and browser edits.
Retain actual action selection, effective profile, wall time and resource use;
separate compilation, test execution and release assembly. Do not attribute a
whole rustc action to the linker or call fewer tests a speedup without reviewing
the retained obligations. No new universal test framework or mutable shared
fixture cache is part of this wave.

The release separately requires the prepared/projection/recovery controls,
verified matching browser assets, joined persona sources and an exact frozen
bundle. Test cleanup must not conceal an outstanding release failure.
