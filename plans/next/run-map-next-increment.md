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

## Instrumentation need revealed by actual artifacts

The historical run's `status.json` now has `phase: { state: "exited" }`.
Existing RunPhase::Exited retains neither root actor nor thread. Its separate
root-binding.json records a thread but no actor identity. Therefore the reader
cannot prove the root actor association after exit from these files alone.
Minimal owner request: retain exact root ActorRef independently of phase in
existing RunStatus (with explicit serialized-version migration), or an existing
launch/admission artifact. Do not add a second registry/log. Root-binding lookup
works when Ready/AwaitingBinding status establishes identity; missing/conflicting
identity remains Unknown. Parent/admission/source/review/integration artifacts
are not derivable from free-text assignment narratives without false certainty.

Context-efficiency evidence: reviewer reported 4m29 Cargo build plus extractor
setup for 0.008s test execution. This is an observed boundary mismatch, not an
estimated saving. Local repeated focused runs are faster but no controlled
cache, provider-prefix, serial/tree, or cost comparison was performed.

Further firsthand guidance gap: while trying to request the one-line owning
status-decoder exposure from root, discovery found no retained root AgentRef in
this child's scope. `AgentRosterEntry` contains IDs, not requestable handles.
Several :type/:info/:doc calls did not resolve a legitimate parent handle;
:bindings confirmed none was supplied. The final typed delivery carries that
integration request rather than forging an actor handle. A documented parent
request handle or explicit root ref in lead assignments would avoid this exact
bookkeeping detour. No savings are quantified.
