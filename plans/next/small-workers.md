# Small-worker TL — gated read-only vertical slice

## Gate and user intent

Do not launch this tranche until root accepts the per-actor service/controller
lifecycle and notification contract. This is complementary to default exact-prefix
specialists, not a replacement. The user wants typed, programmable small workers,
same parent pane, shared parent worktree, reusable .hs tools/specifications and
explicitly selected context. Existing `plans/small-agents.md` is the broader design;
this document supplies the bounded implementation handoff without old conversation.
No small-agent API described here is assumed implemented.

## First outcome

A parent supplies a typed numeric discrepancy to a read-only small worker with a
stable narrow tool vocabulary. Worker classifies/minimizes; parent independently
validates result. Deterministic numeric execution remains ordinary code. Larger
semantic ambiguity goes to an exact-prefix specialist. Use existing accepted numeric
fixtures/oracles, not a second engine. Sol Low is an initial option to evaluate,
not a universal cost/quality claim; respect available runtime model/effort controls.

## Structural contract

- Typed input/result, explicit `input -> Text` renderer, bounded execution policy,
  stable tool interface, explicit read-only workspace/capability policy.
- Same worktree does not grant ambient parent access; Rust authorizes child principal.
  Exported closures cannot smuggle parent authority. First slice has no shared writes.
- Separate actor/request/provider identities and attribution. Same pane means one
  presentation owner, not multiple TUIs writing the same terminal. No dashboard
  project here; coordinate presentation hook with shoal-repl, expose minimal events.
- Existing actor scheduling, requests, watches, cancellation and service connection;
  no second agent manager/mailbox/provider loop or Haskell subprocess launcher.
- Use on-disk .hs specs through existing compiler/workbench; capture definition and
  source version. Later edits do not mutate admitted work. No promise that arbitrary
  closures survive restart. Host-tool calls must not deadlock against busy parent.
- Explicit context mode; small input does not mean weak authorization. Keep common
  tool/base prefix stable across a task family rather than bespoke schema per case.

Owners: `tidepool-actor`, `tidepool-agent`, `tidepool-runtime`, `tidepool-node`,
`tidepool-worktree`, actor_host/host_dynamic_tools and Haskell library. Read current
AGENTS and newly accepted service contract. Root owns manifests and shared API;
service implementer retained for integration questions. Choose exact admission API
with root after inspecting existing role/authority and context-rendering owners.

## Recursive waves / acceptance

Scaffold one real read-only consumer and explicit unsupported holes. Split authorized
context/tool export from scheduling/presentation adaptation only after signatures
and ownership are stable. Fresh reviewer traces capability rejection, cancellation,
partial failure, exactly-once settlement, definition capture, parent tool reentrancy
and attribution. No unconstrained writer surface to make the demo convenient.

Bound the case set; compare independently accepted quality, latency and recorded
usage against deterministic execution and an exact-prefix specialist where useful.
Actual normalized input/cache evidence, not fork metadata, supports cache claims;
keep tracing opt-in/private/bounded. Report overhead or negative results honestly.
Deliver candidate, reusable .hs specification, real typed result, verified rejection
and cleanup paths, tested revisions and remaining limits. Expand writes/model variety
only in a later explicit decision, not as a prerequisite for this slice.
