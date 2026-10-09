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
  Retaining a complete native carrier must preserve its available exact child
  carriers for unselected bodies. Keep that byte custody separate from source
  roots, compiler input roles and executable group demand; later requests can
  select another body without recreating its issuing dependency versions.
  Published originals retain their issuer's immutable artifact/group selection
  separately from later executable demand. Reopening and recovery validate that
  proof against exact retained custody; a broader inventory cannot recreate it.
  Recovery records interface edges and executable group demand; native edges
  derive from original certificates. A completed source-compilation instance
  proof remains transaction-owned rather than reconstructed from durable bytes.
- Pure activation previews preserve the original instance graph separately from
  native availability. Authored dictionary bodies use retained originals matching
  that capture's canonical interfaces; type custody alone cannot replace native
  bytes, and source-original Core recovery must not replay authored declarations.
- Validate compiler completion against the issued request roles, including private
  native availability. Its persistent declaration snapshot retains independent
  lexical and type roles and cannot validate a promoted request artifact closure.
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
