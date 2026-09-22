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
- `tidepool-extract-cmd` stays the small invocation builder. Do not move cache
  or runtime policy into that dependency leaf.
- Cache tests must cover invalidation, relocation, warnings, and uncacheable
  negative cases, not only hits.
