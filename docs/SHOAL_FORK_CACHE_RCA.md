# Shoal fork boundary and cache investigation

## Findings

The original report used Codex `702639b`, Sol low. Its first three child
responses cached 0, 0, and 14,720 tokens out of roughly 19,495 input tokens.
The retained histories agree on their invocation boundary and declarations.
They do not contain the exact provider requests, so they cannot establish why
the provider missed. The effort-control change is not established as the cause;
mixed first-request cache outcomes also occurred before that change.

Two concrete failures were independently identified: the Haskell tool description
exceeded the provider's 1,024-character limit, and Sol rejected Astra's
`configuration_update` input type. The description is shorter and checked at
Rust compilation time. Codex `702639b` restricts that configuration item to Astra.

The former unfold boundary captured a tool invocation while it was still running.
Children needed synthetic result closures and inherited the Haskell scope before
subsequent statements. The new boundary admits children immediately, then launches
them after the actual enclosing tool result is durable and its open tool batch
is closed. The children share the final committed Haskell scope. A later statement
failure preserves successful admissions and is included in the inherited result.
This fixes the fork contract; it does not promise a provider cache hit.

## Prewarming

Startup WebSocket prewarming issues a non-generating request. Its usage was not
part of ordinary response observations. That omission is now logged explicitly,
and provider cache diagnostics/options survive response parsing and persistence.
Forked sessions skip the startup prewarm; their first request carries inherited
history. Root-session prewarming remains unchanged.

A no-tool control with three siblings cached 17,280 tokens on every first child
request both with and without fork prewarming. The prewarms themselves reported
zero cached tokens. Thus neither a prewarm miss nor its timing establishes the
cause of the original child misses.

## Live boundary comparisons, 2026-09-05

All runs used Sol low and an identical reference-data question for the children.
Each comparison first rejected `afterCallId` while the parent call was pending,
then tested three `throughCallId` children, supplied the real result, and tested
three `afterCallId` children. The model made no filesystem changes.

| Ownership | Boundary | First child input tokens | First cached tokens |
| --- | --- | --- | --- |
| One app-server | Invocation | 19,051 each | 18,816 / 17,280 / 18,816 |
| One app-server | Completed result | 19,003 each | 18,816 / 17,408 / 17,408 |
| Separate app-servers | Invocation | 19,080 / 19,080 / 17,458 | 17,152 each |
| Separate app-servers | Completed result | 19,032 / 19,032 / 17,410 | 17,152 each |

The separate-process completed-result siblings share ten exactly equal input
items, including the real tool result, and contain no child-local result closure.
Their canonical JSON prefix SHA-256 is
`ef2f06255d7e0fbcfbb1564272e755e550c9bafced68fbf5b40d8edce9481f82`.
The invocation siblings share nine items, ending before their synthetic results.
The extra 1,622 tokens in some children are refreshed skills instructions
appended after the common prefix; they do not change that prefix.

Artifacts on the investigation machine:

- `/tmp/shoal-cache-rca/completed-call/`: same-process requests and outcomes.
- `/tmp/shoal-cache-rca/completed-call-processes/`: separate-process requests and outcomes.
- `threads.json`: parent and child IDs; `*.jsonl`: app-server notifications.
- `traces/`: opt-in existing Codex request/response traces.
- `/tmp/shoal-cache-rca/completed_call_probe.py` and
  `completed_call_process_probe.py`: executable probes.

The original zero-cache outcomes have not been reproduced in these controls.
The provider returned unavailable diagnostics for the observed root responses;
child responses supplied no diagnostic reason. A causal claim about those
original misses remains unsupported. A recurrence should retain the opt-in
request traces and response IDs alongside first-response usage, rather than
inferring provider behavior from Haskell snapshot identity or cumulative usage.

Shoal forwards the opt-in `CODEX_ROLLOUT_TRACE_ROOT` environment variable to its
actor processes. Choose a path writable inside their execution environment
(for example a directory under the mounted Codex home). These traces contain
full request content; default runs retain only their existing observations and
the added provider cache metadata.

## Packaged Shoal smoke, 2026-09-05

The Nix `codex-host-tools-contract` check passed for Codex `c8460ff`, including
protocol v3 and `--after-call`. A disposable Shoal session used that packaged
binary with Sol low and the current host, run
`20286309-7ec6-4663-b069-831b123ddb47`.

The root admitted two research children, rebound `smokeMarker` to
`after-completed-block` after unfold, and registered their watch in the same
tool block. In a later tool call it rebound its own marker to `later-parent-only`.
The authoritative watch returned both child values as `after-completed-block`;
Haskell equality checks returned `Just (True,True)`. Both child worktrees had
source HEAD `b7c84842849630d157078c8e71eee18d270613aa`.

First child requests share 19 exactly equal input items through the actual
result of `call_JSEkTUPNOoovd6zJrFttXNGP`. Their canonical JSON prefix SHA-256 is
`bb68ca4431bef5fb3081a81aafacbb6fe5afe38ed69d446937bc68e1cdeca546`.
Neither contains the later parent rebinding, a synthetic tool closure, or an
interruption input. Both retain root cache key
`01a073bf-51ba-7ef0-9952-59dee6623e72` and start with a generating request.
The TUI replay displayed a generic interruption banner at the fork point;
this was not present in provider input and did not prevent typed settlement.

Artifacts: `/tmp/shoal-completed-tool-smoke/.shoal/logs/` and
`/home/inanna/.codex/shoal-fork-smoke-traces/`. Child threads are
`01a073c1-1e22-78a0-b7c4-ba5d9fea1787` and
`01a073c1-1e9e-7573-b363-a0cf862ab723`. This smoke establishes the hosted
completion boundary and scope semantics, not a cause for the original cache miss.

The disposable tmux session was stopped after settlement. Its host and actor
processes exited, and a nonblocking acquisition confirmed the binding lock was
released without deleting the lock file. Worktrees and trace artifacts remain
available for inspection.

## Verification

`just test-lib tidepool 'test(actor_host)'` passed all 30 tests (51 excluded),
nextest run `b2d2efb6-0bee-456e-8de4-f53df5904ba4`, in 239.762 seconds.
The earlier run exposed a fixture dispatcher that omitted the new completion
acknowledgement; it now shares the acknowledged script dispatcher. Coverage
includes typed reply/watch settlement, final-scope inheritance, later tool
failure, multiple admissions, rich values, and reattachment cancellation.
GHC-backed host tests are assigned to the existing limited-concurrency group.
Formatting and `git diff --check` passed. This is focused boundary verification,
not a full workspace battery.
