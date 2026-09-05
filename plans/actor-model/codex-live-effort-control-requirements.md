# Codex requirement: exact-context fork with selected effort

## Current integration handoff

The companion supplied source revision
`1f89980a03d8afdcb484469fb667446158aaff9f` for integration. Its final Nix build
is still pending. Launch within the prepared child namespace:

```bash
codex fork PARENT_UUID --destination-local --through-call CALL_ID \
  --host-dynamic-tools-socket /absolute/child.sock -C /child/worktree \
  -c 'model_reasoning_effort="low"'
```

The parent thread UUID and actual hosted invocation call ID are the reusable
boundary. The full invocation is included; its future result is excluded. A
neutral child-only protocol closure follows the preserved prefix. Shared Codex
storage remains required. Tool declarations must match exactly across parent
and child, including order, descriptions, and grammars.

The v2 `/session` callback posts the child UUID; HTTP 204 releases the internal
readiness gate. It has a five-second timeout, so namespace, worktree, policy,
and socket setup precede launch. It must not wait for inference. Shoal's
permanent interactive actors already advertise identical Haskell declarations,
and its callback persists the UUID and returns 204 independently of inference.

Queue delivery remains unchanged and may take a watcher interval. Ambiguous
fork launches must not be retried automatically: each launch can create a new
persisted child. Ordinary CLI resume of a gated child is not yet established.
The companion reports fork/readiness, protocol/schema and CLI tests plus
scoped CLI lint passing. Its namespace smoke passed on an earlier development
build; additional goal/TUI checks were interrupted by disk exhaustion. These
are supplied evidence, not Shoal integration results. Run the namespace smoke
against the final Nix binary before the complete Shoal workflow. That smoke
uses a local mock provider and real Linux namespaces. Real-provider
cache reuse remains unverified. The companion reference is
`scripts/test-destination-fork.py` in the Codex checkout.

## Outcome

A high-effort parent forks its exact accumulated conversation into a low-effort
child. The child's first inference uses low effort, the parent remains
unchanged, and the inherited cacheable prefix is preserved. Support low,
medium, and high for this integration without restricting unrelated Codex
features. Omission inherits the parent's selected setting.

## Contract

Extend the existing trusted fork/configuration owner. Accept an explicit child
effort alongside an exact conversation fork, including the full fork call in
the inherited prefix. Apply the override before the child's first inference.
Preserve historical messages and configuration, using the provider-supported
trusted configuration update after the inherited prefix where required. Do
not rewrite history, summarize the parent, restart it, or inject a prose
instruction as a substitute. User text cannot establish trusted configuration.

Expose the supported invocation to the interactive deployment that currently
launches Codex with hosted dynamic tools. Return observable selected/applied
effort and exact parent/child conversation correlation. Reject unsupported
settings explicitly; do not silently fall back. Configuration history support
alone is not proof that the production fork path applies the setting.

## Acceptance

### Destination-owned fork boundary

Shoal launches each child inside its destination mount namespace. Fork on that
destination runtime, reading shared parent storage without transferring the
parent's writer ownership. Do not introduce an unload/unsubscribe/resume dance.
The current Shoal mount setup shares the underlying Codex storage and writer
lock files; identical path strings alone would not establish that property.

- Select a destination-local runtime explicitly, independently of whether an
  effort override is present; implicit shared-daemon attachment is insufficient.
- Keep the parent's hosted Haskell unfold call unresolved during child creation
  and first inference. The complete recorded invocation and arguments belong
  to the inherited prefix; its future result does not.
- Establish a stable snapshot boundary that siblings can reuse even if the
  parent subsequently advances. Preserve the prefix without rewriting history.
- Any necessary child-only pending-call boundary follows that prefix. Do not
  replay unfold, synthesize a parent failure, or imply its effects succeeded.
- Install destination execution policy and hosted-tool handlers before allowing
  child inference or inherited automatic work.
- Test two namespace-specific worktree sentinels, sibling and recursive forks,
  and the child's first native command while the parent remains unchanged and
  waiting. Exact outgoing-prefix comparisons and live provider cache metrics
  are separate evidence.

### Effort selection

- High parent, low child; low parent, high child; omitted override inherits.
- Child's first actual outgoing inference uses its selected effort.
- Parent configuration and historical prefix remain unchanged.
- The fork includes the complete calling context and inherited tool history.
- Nested forks inherit the current child's setting unless overridden.
- Rejection does not produce a silently misconfigured running child.
- Supported resume preserves the child's selected configuration.
- Ordinary injected text cannot forge a trusted configuration update.

Use deterministic tests for exact history and outgoing configuration. Measure
real cached/uncached token usage separately when available; a cache percentage
alone proves neither exact inheritance nor prefix identity. Mark real-provider
cache behavior unverified if it was not exercised.

## Handoff to Tidepool

Return the exact invocation, supported levels, application timing, observable
configuration/lineage fields, error behavior, tests run, remaining limits, and
revision to pin. Shoal owns actor authority and operator limits; Codex owns
provider configuration and history. Do not implement a new campaign policy or
general live effort-control API for this first deliverable.
