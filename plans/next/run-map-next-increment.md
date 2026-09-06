# Run-map next coherent increment

Accepted partial inventory baseline 607dc219 is incorporated by fast-forward,
preserving this lead's earlier commits. Root's verification was pending when
this assignment arrived; local verification is separate.

This increment owns only run-map module/example and its tests/docs:

- Resolve root actor identity from existing typed RunStatus (never actor0 by
  convention), link explicit root-binding thread where identity is observed,
  and retain Unknown on unavailable/conflicting evidence.
- Optional inclusive-from/exclusive-until UTC Unix-millisecond window on
  timestamped inbox events. Static actor inventory remains visible; untimed
  events remain visible with unknown window membership, not invented timestamps.
- Typed request-presentation and watch-transition metadata creates only edges
  that actual structured inbox fields support. No parsing assignment prose for
  parentage, source commits, acceptance or review. Those edges remain Unknown.
- Extend the representative example's Rust argument parsing, not shared Shoal
  launch/CLI owners. Provide exact separate integration request.

Usage integration remains with the existing backend owner. The present
InteractiveBackend.observe seam exposes first/latest/full-history summaries,
not bounded-window per-response records. A retained request to service asks for
a disjoint extension of that owner, rather than creating a duplicate parser.
No blind launch retry. Independent reviewer should inspect the exact next
candidate after these behaviors and focused negative-path tests exist.
