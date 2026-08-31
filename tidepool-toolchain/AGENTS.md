# Toolchain and compilation policy

This crate owns toolchain/stdlib discovery, deploy compatibility, shared paths,
compile diagnostics, and the one policy-bearing artifact compile front door.
It sits below `tidepool-runtime` and must not depend on runtime/session errors.

- All compilation policy enters through `artifacts`; do not add a cache-free
  alternate frontend.
- A cache key must describe the exact invocation and every readable input.
  Unknown flags or unenumerable inputs make an invocation explicitly
  uncacheable rather than silently under-keyed.
- Keep eval keys path-identity-sensitive and relocatable invocation keys based
  on root-relative module identity. Normalize diagnostics before caching so
  temporary absolute paths do not escape.
- `.hs`, `.hs-boot`, `.lhs`, and `.lhs-boot` share one dependency-manifest
  policy. CPP inputs remain uncacheable until their dependency closure can be
  proven.
- `tidepool-extract-cmd` stays the small invocation builder. Do not move cache
  or runtime policy into that dependency leaf.
- Cache tests must cover invalidation, relocation, warnings, and uncacheable
  negative cases, not only hits.
