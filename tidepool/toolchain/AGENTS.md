# Toolchain and compilation policy

This crate owns toolchain/stdlib discovery, deploy compatibility, shared paths,
compile diagnostics, and the one policy-bearing artifact compile front door.
It sits below `tidepool-runtime` and must not depend on runtime/session errors.

- All compilation policy enters through `artifacts`; do not add a cache-free
  alternate frontend.
- A cache key must describe the exact invocation and every readable input.
  Unknown flags or unenumerable inputs make an invocation explicitly
  uncacheable rather than silently under-keyed.
- Use one immutable recipe and named bundle. Authored dependencies retain path
  identity; generated source markers exclude scratch-directory identity.
  Normalize diagnostics before caching so temporary paths do not escape.
- Compiler evidence owns consumed bytes and import-resolution witnesses.
  Missing, incomplete, or incompatible evidence cannot yield a cache hit.
  CPP, TH and other untracked inputs remain uncacheable until proven complete.
- `CertifiedSourceSelection` owns compiler roles. Full artifact custody and
  `recovery_products` may retain several native versions of one module;
  authored consumers use the issued `SelectedOriginalClosure`. Validate its
  native dependencies through exact artifact/group edges, including historical
  children, rather than flattening custody into a module map. History tests
  must carry certification through the production consumer that needs selection.
- `tidepool-extract-cmd` stays the small invocation builder. Do not move cache
  or runtime policy into that dependency leaf.
- Cache tests must cover invalidation, relocation, warnings, and uncacheable
  negative cases, not only hits.
- Session salts and injected session interfaces bypass the artifact cache.
  Inside the resident compiler daemon, dependency-validated memo entries can
  still reuse immutable support modules; this module memo is distinct from the
  Rust artifact cache. Use the matched measurement tests as evidence for cost
  and savings, not projected estimates.
- `build_prepared_fixture` is the build-action mode of that same front door.
  It requires action-owned current-directory scratch and declared source roots,
  uses the configured direct compiler, and has no runtime cache authority.
  Export portable prepared programs and metadata only; source-bound compiler
  certificates never move into fixture resources or get restamped.
- `build_deployment_module_package` shares build-action isolation and exports
  authenticated module records through the existing catalog owner. A declared
  probe and ordered targets select the cohort; deployment export never grants
  runtime candidate publication. Schema 4 keeps original source paths and
  proofs unchanged; product references resolve under the opened catalog's
  canonical parent. Source retention remains the qualification owner's job.
