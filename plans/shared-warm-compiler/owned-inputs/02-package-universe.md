# Assignment 02 — own the consumed package universe

Base: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. Future implementation assignment; no pinning, toolchain change or executed check is authorized by this document alone. Follow [supervisor integration](README.md) and coordinate shared Haskell/protocol edits with warm-workers.

Primary goal: stop repeated package-interface reads, hashing and sidecar decoding only when a real owner guarantees that host verification and GHC consume the same immutable package bytes. Secondary questions are lazy/transitive package loading and complete home-owner negative dependencies. Do not turn a successful path/hash observation into a cross-request lease.

## Source and the missing relationship

`tidepool/toolchain/src/recovery_artifacts.rs::PackageInterfaceValidation` shares observations within one operation. `verify_with_budget` opens a selected path, reads/hash-checks bytes, distinguishes missing/unreadable, and retains a bounded byte copy. `validate_package_import_evidence_with_validation` authenticates the owning interface/sidecar and visits package roots. Many callers construct fresh default validation owners, so a logically shared universe may be read again at materialization, certification, admission and recovery boundaries. A cache hit within one validator does not prove those other operations reuse it.

`bridge/haskell/src/Tidepool/PackageWitness.hs` owns `PackageImportRoot`, `PackageImportEvidence`, `AdmittedPackageImports`, `selectedPackageInterface`, `packageImportRoot`, `withPackageRootObservations`, `validatePackageImportRootWithin`, `validateAdmittedPackageSelection` and terminal `revalidateAdmittedPackageImports`. Current sharing deliberately retains observations, not resolver answers. `selectedPackageInterface` consults GHC's finder/unit environment; expected witnesses include unit/module/path/seal. New environment selection is checked independently of old byte evidence.

`ExactScope.hs::{readExactScopeWithOwner,validateExactScopeEnvironment}` authenticates selected sidecars and checks package selection at receiving boundaries. `ExactHydration.hs::{PackageFinderFacts,newPackageFinderFacts,forkExactContextWithPackageFacts}` shares package finder facts with fresh home/finder state. `GhcPipeline.hs::{CompilerUniverse,withResidentCompilerScopes,residentCompileOne}` owns already loaded package/EPS/interpreter state and lazily retained interface environments. A new host byte snapshot after those objects loaded does not prove their provenance.

Compiler/deployment discovery belongs to `tidepool/toolchain/src/toolchain.rs::{CompilerDeploymentAuthority,AdmittedCompilerDeployment}` and the existing endpoint/deployment owner. Frozen package qualification already owns Nix closure rooting and verification in `build/package/qualification.py::{pin_nix_closure,verify_registered_gc_roots}`. Inspect those contracts; do not move their policy into a package validator or edit infrastructure to invent a lease.

## Decision: source-backed mode or explicit observation mode

Plan two typed internal states; the eventual names are implementation choices, and their constructors must remain with the issuing owner:

| State | Source owner and consumption | Permitted reuse |
|---|---|---|
| Source-backed package universe | Exact admitted compiler/deployment source owns a retained immutable Nix closure, with actual GC-root/retention lifetime. Host and worker boot select that same closure's compiler/libdir/package DB/unit configuration and package interface/native library paths. Actual selected/loaded bytes must be traceable to it. | Authenticate package facts once per owned source/format/selection, reuse decoded immutable facts and body references; keep request-local unit/home selection checks. |
| Operation-scoped package observations | Any external/mutable package path, unknown retention, previously warmed universe of unproven provenance, or inability to establish actual GHC consumption from source owner | Existing fresh scoped read/hash validation and current finder checks. Share observations inside their valid operation and pure authenticated decoding where safe. No cross-request byte-validity answer. |

The preferred source is the existing qualified retained compiler/deployment closure, not another global package cache. Its source identity includes compiler producer, package DB contents and order, selected unit IDs/ABI, libdir, relevant flags/plugins and native package closure. Source custody must outlive every lazy decode, cached checked world and interpreter use. A frozen descriptor or NAR/digest record without its retained source lifetime is not sufficient. Package roots outside the acquired closure make that portion unqualified for source-backed mode; do not silently classify them by pathname prefix.

This is conditional on an actual same-byte proof. First try to use the already retained immutable source and exact finder selection. If GHC can still load package interfaces/libraries from another mutable location or warmed EPS contains unknown inputs, do not enable pinning. If supplying owned interface bytes would require redirecting GHC package loading, specify that integration with the compiler owner and test it before claiming success; capturing a side copy the compiler never reads is not the solution. Do not rewrite package paths in certificates merely to make comparison pass.

Observation mode is the functional fallback, not a hidden performance success. Record why source-backed mode is unavailable, remaining per-request reads and their owners. The supervisor may accept row/transport progress while marking package reread elimination unqualified. Fresh observations do not bless previously loaded EPS, lazy package interfaces or native code: every reused decoded object must retain the original observed selection/body owner that actually supplied it. If current observation changes that association or its provenance cannot be established, refuse reuse or boot a clean admitted universe before accepting user source. Finder path equality and a newly observed hash cannot attach new bytes to old GHC objects. Upgrading from observation mode likewise requires a newly issued booted universe; never graft an owner token onto already active unknown package state. Source-owner loss/refusal after request acceptance follows current settlement, not automatic execution replay.

## Wave A — prove the source boundary before deleting reads

Inventory all Rust `PackageInterfaceValidation::default` and Haskell package observation constructors reached by the real warm-cell/lookup/registration path. Record each physical read/hash/decode, selected unit/path, lifetime and consumer. Include package reads hidden in native certification and canonical/original products. Separate sidecar authentication, byte acquisition and current resolver selection; they are different facts.

Design one issuance path at admitted compiler/deployment acquisition, before `withResidentCompilerScopes` boots GHC. Carry the same source owner to host validation and the worker's fixed package universe. Extend existing deployment/worker handshake only with warm-workers' assigned protocol owner. A new worker instance or source changes creates a new universe association; daemon slot/PID alone is not universe identity. No per-actor mutable package registry should arise.

The proof must explain:

1. Which owner keeps compiler/package DB/interface/native library source alive and immutable, and its exact release mechanism.
2. How actual `selectedPackageInterface` and GHC interface/EPS loads resolve to those source bytes, including indirect/lazy imports and initially loaded packages.
3. How native interpreter package code and plugin/TH dependencies are kept within the same source contract or cause observation-mode fallback. The lease is not permission to replay compile-time effects.
4. How host validates the source owner's selected immutable body rather than rereading an unrelated old path; how unchanged bytes under a different unit/DB/home selection remain distinct.
5. What changes invalidate it: package source/DB order, compiler producer, flags/plugin policy, worker incarnation, loader epoch where relevant, or source release.

Independent review primary goal is actual same-byte consumption. Secondary questions: lazy GHC references surviving source release, resolver-negative dependencies, and fallback coverage. If proof cannot be made from concrete source/consumer operations, keep observation mode and return the blocker instead of deleting terminal checks.

## Wave B — carry authenticated facts through consumers

Only after A is accepted, reuse admitted immutable package bodies/decoded sidecars through `PackageInterfaceValidation`, certification/materialization and `PackageWitness`. Extend the existing owners instead of adding a path→valid Boolean or independent package inventory selecting roots. Thread the explicit source association through actual domain constructors; raw disk/recovery acquisitions still authenticate bytes and errors as required.

Keep current environment selection checks. Host-inputs' complete home-owner membership remains independent of optional input roots: adding a home owner can invalidate a previous package answer even when all package bytes stay identical. Dependencies must include the queried absence/membership and exact unit/path/source selection. The worker cannot preserve a package choice merely because its cache contains the interface. Runtime private/native choices, Empty/Partial/Full availability and exact required children stay with their existing issuers.

Reuse raw byte authentication only for owned source. Authentication of a fresh sidecar/owning interface remains necessary at its issuer even if its package dependencies have stable bodies. Validate fresh outputs once and carry that answer, not by asking every later materializer to repeat it. Coordinate fresh output package-evidence production with 01 so `.hi.packages` stays coupled to its actual value/interface owner.

Fold the accepted source-mode/fallback implementation, then review the integrated worker registration → checked compile → host admission → P6 → later lookup path. Deleting bytes work only in preflight while terminal validation still rereads the entire closure is incomplete; changing terminal validation without same-byte ownership is incorrect.

## Tests and falsifiers

Use real package sidecars emitted by `PackageWitness::sealPackageImports` and consumed by production Rust/Haskell validation, not a test-only “pinned” factory. Existing Rust controls include `package_validation_stage_keeps_one_capture_and_refuses_conflicting_digest`, `package_validation_new_stage_rechecks_path_and_inode_replacement`, `package_validation_budget_falls_back_to_fresh_reads`, `package_validation_keeps_regular_file_and_size_bounds`, and `package_sidecar_binds_owner_and_selected_package_bytes` in `recovery_artifacts.rs`. Retain their intended refusal semantics for observation mode.

Extend the real source-boot histories in `bridge/haskell/test-source-boot/SourceBootCases.hs` that count package preflight requests/physical reads, mutate installed fixtures, check resolver alternatives, and exercise mutation between admission and publication. Discover the generated native target/leaf names at the implementation revision; do not guess labels from filenames.

Required joined controls:

- Two owners referencing the same package interface reuse physical observation/authentication only inside the established owner lifetime; contradictory expected seals, changed owner/unit/path and duplicate roots refuse.
- Mutate/replace a controlled external package path after one successful request; the next observation-mode request rechecks and refuses/selects newly admitted facts correctly. Missing and inaccessible remain distinct. Tests modify isolated fixture package trees, never installed global/Nix files.
- In source-backed mode, host reads and real GHC decode/use produce the same authenticated body identity from the retained source. Keep a lazy dependency unforced until after releasing the original caller; the lease must survive. Changing package source/DB selection issues a new universe and prevents old reuse. A source-reference token without the backing lease must fail issuance.
- Start a warmed unknown universe, then attempt to attach a new source owner: refuse/fallback or boot a separate admitted universe before work; do not bless old EPS by matching its path/seal.
- In a controlled isolated package tree, load same-unit/same-path version A into EPS, retain an unforced lazy dependency, replace the file with B, then select A again. Host receipts, actual GHC type/instance use and native package body must agree with the owning version at each step or refuse before source acceptance. Fresh byte observations alone must not relabel old EPS/native objects; include both lazy forcing and loader use after the switch.
- Add a home owner with the same unit/module as a previously resolved package import. Preserve negative dependency invalidation and current selector checks even with no package-byte changes. Keep optional-root policy out of this census.
- Use a compiled package through ordinary typechecking and a real package-code/renderer consumer where declared; verify the loader did not consume an alternate body. Unsafe CPP/TH/plugin/external dependencies retain their existing replay restrictions.
- Drop one actor/source snapshot while another still needs the package owner, then drop the last. Observe actual root/FD/source release and lazy use refusal after true retirement; a cache row count alone is insufficient.

Counters at each physical owner: package file opens/read/hash bytes, immutable body acquisitions, sidecar/owning-interface decodes, distinct resolver calls and selected owners, universe issuance/reuse/fallback reason, terminal revalidation, source leases and package loader activity. Compare cold versus repeated same universe, not 100 reads inside one temporary validator. Capture totals across every hidden validator constructor. A pinning success should remove repeated byte work without removing actual receiving selection checks. If memory retention or source lease costs exceed savings, report that tradeoff rather than promote an unmeasured cache.

Compile changed Rust/Haskell consumers, run their focused counted checks, and run the supervisor's real shared-workspace actor history at the integrated candidate. Retain source OID, exact package source/closure contract, commands, nonzero counts, logs and source-backed/fallback classification.

## Deletion and stop conditions

Delete only the superseded repeated byte observations proven unnecessary under source-backed mode. Keep observation-mode entry points private and explicit; remove broad path/hash memoization and duplicated package ownership if introduced during migration. Replace stale comments claiming pinning where source consumption is unproven. Do not preserve two independent validators that issue contradictory facts.

Stop/escalate on an unowned package source, mutable path not consumed from owned bytes, missing lazy/native dependency coverage, inability to distinguish current home/package resolution, new deployment/infrastructure retention requirements, or stale/indeterminate accepted work. The recorded fallback is correct current validation with known remaining cost; neither unsafe pinning nor a guessed memory/process policy is an acceptable substitute.
