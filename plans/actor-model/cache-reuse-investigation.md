# First-inference cache reuse investigation

Date: 2026-09-04. Review the working trees in Tidepool and `../codex`.

## Result

The provider's transport session routing was not following inherited cache
context. Preserving `prompt_cache_key` alone was insufficient. Two live probes
that also used the inherited UUID in both the WebSocket `session-id` header and
top-level request `client_metadata.session_id` substantially improved first-child
reuse. Changing either transport location alone did not.

The final implementation now persists a typed `ProviderCacheAffinity` record,
separate from runtime identity, and applies its routing UUID consistently to
headers and request metadata. A fresh production-path canary passed without diagnostic switches: first-child
reuse was 16,512 / 18,135 (91.0%).

## Measurements

All actual campaign actors below use Astra. Roots use medium effort. The
same-effort control also uses medium; other children use low.

| Control | First child cached / input | Result |
| --- | ---: | --- |
| Original destination fork | 7,808 / 24,745 | Independent cache keys |
| Inherited cache key | 7,808 / 24,751 | Same miss despite shared key |
| Request-traced low child | 7,808 / 18,104 | Initial input objects exactly inherited |
| Request-traced medium child | 7,808 / 21,439 | Miss also occurs without changing effort |
| Header only | 7,808 / 23,489 | Insufficient |
| Header and request metadata | 23,296 / 25,179 | 92.5% on first inference |
| Request metadata only | 7,808 / 26,854 | Insufficient |
| Header and metadata, repeated | 23,296 / 28,529 | Improvement reproduced |
| Final implementation, fresh root | 16,512 / 18,135 | 91.0%, no diagnostic switch |

Later child turns commonly reuse about 99% of input. These are **within-child**
hits and must not be reported as proof of parent-prefix reuse. Current Shoal
lineage observations explicitly describe the last provider response, so that
surface alone cannot answer the first-inference question after a child finishes.

The HTTP diagnostic root also had 7,808-token hits on some successive full
requests despite an unchanged initial prefix, interspersed with much larger
hits. It did not establish the HTTP child result: the temporary custom provider
could not be resolved during destination bootstrap, including after defining it
in the diagnostic project's configuration. Two failed diagnostic groups retained
their worktrees. They executed no child inference.

## What was verified

Codex's existing rollout trace records the actual outgoing requests, including
WebSocket deltas. No new request-capture subsystem was added.

The traced low child's initial request contains exactly the same first eight
input objects as the parent's first request: tools, base instructions, other
developer/user messages, and the initial medium configuration update. The
request-level reasoning baseline, model, and cache key also agree. The low
configuration update follows the inherited prefix.

Reconstructing the later parent prefix from WebSocket requests and responses
reveals one metadata difference: a returned assistant message gains
`content_item_kinds: ["unknown"]` in durable history. Its text and identity are
unchanged. This is after the entire initial input; it does not identify an early
prefix divergence that explains the observed cutoff. Do not equate this traced
comparison with access to the provider's normalized token stream.

The parent usually continues an existing WebSocket response chain. Each child
starts an independent connection with a full request. Request metadata correctly
has independent thread/session identities. No `x-codex-turn-state` routing token
appears in the traced parent request metadata. The provider's worker placement
and token-level cache key are not observable in these artifacts.

## Local change retained

Before the investigation, provider routing and prompt cache selection defaulted
to each independent CLI session's identity. `SessionMeta.cache_affinity` now
stores a typed routing session UUID and a separate prompt cache key. Both follow
fork/resume history. Legacy metadata derives the source's former selection;
existing dedicated internal/guardian prompt cache buckets remain intact.

The routing UUID is deliberately distinct from arbitrary prompt cache key strings.
Codex's actual thread/session IDs, ownership, and agent budgets remain unchanged.
Full identity remains in `x-codex-turn-metadata`, while `thread_id` continues to
identify the individual child. Both HTTP and WebSocket transports use the same
cache routing contract. No diagnostic environment switch remains in the source.

Migration decision: the intermediate cache-key-only field was an uncommitted
diagnostic format. It has no compatibility adapter; those local diagnostic
rollouts use the normal legacy fallback. The final optional record provides
backward-compatible reading of released rollouts without cache metadata.

Routing events use the `codex_core::cache_routing` log target and include actual
thread/session identity, transport routing identity, and prompt cache key. Full
request content remains in the existing opt-in rollout trace facility.

Focused checks passed:

- Direct and wrapped destination namespace smokes, including siblings and a
  grandchild, with equal inherited cache keys and isolated native execution.
- Four app-server integration cases covering legacy/paginated fork and resume,
  never-inferred children, and inherited-goal deferral/readiness.
- Existing dedicated internal-session cache-selection test.
- 299 app-server protocol/schema tests; stable and experimental exports rebuilt.
- CLI build, formatting, and diff checks. No full workspace battery.

## Evidence identifiers

- Final canary run: `21ea80d8-1b00-49fc-b76e-012ec457e506`.
  Root `01a0701b-8b5f-7182-85ec-871e0a34a89b`;
  child `01a0701c-ef2a-7ac1-ac4c-3a8c368bfa8c`.
  Inherited proof `shoal-astra-canary-6b91::child`; repeated typed polls,
  isolated sentinels/mounts, and dirty-worktree-preserving cleanup passed.

- Shared-key canary run: `28c31e3a-0041-4caa-86a6-91d3344d8259`.
  Root `01a06ffa-4cc7-7de1-a1b7-8a0a4ac632e2`;
  child `01a06ffb-74f5-75a2-a489-41536e08eee1`.
- Traced canary run: `acdb3452-9826-43f4-adf6-c7e62a6c62b6`.
  Root `01a06ffd-85bd-7ed3-8ec0-6e433e31dbae`.
- Low child: `01a06ffe-bc34-70a3-9e09-8ddbd8189aac`;
  first response `resp_0ddd9e1b34da42b1016a9ba59ab42087d1bd3f497977c99a1f`.
- Medium child: `01a07002-9182-75e1-89f1-34974322b66d`;
  first response `resp_0ddd9e1b34da42b1016a9ba6967f4c87d1a2184b3039c5f22e`.
- HTTP diagnostic root: `01a07001-9dc3-73b0-992e-6f11d2fde2ad`.
- Local request traces: `/home/inanna/.cache/tidepool/cache-fork-traces/`.
  These contain full campaign inputs; report hashes and selected fields rather
  than publishing whole bundles.

Both traced successful groups completed typed watches and deliberate cleanup,
retaining worktrees and Git history. Their roots remain idle and retained.

## Acceptance boundary

### Packaging handoff (2026-09-05)

Implementation is committed as Codex
`118e1cfcd1d7dd120460ff0685e976f0d17327dc` and Tidepool
`117134e6182cfd6d71e447a6370f0bccdfb8b69c`. Tidepool's flake lock pins that
Codex revision. The fresh real-provider canary above used the source-built CLI;
it is not evidence that the Nix package has passed its checks.

The parallel Nix release build was killed by the kernel for memory exhaustion.
The serialized retry is still running at this handoff, with the CLI crate
actively compiling. Its command is:

```sh
nix build .#checks.x86_64-linux.codex-host-tools-contract \
  --cores 1 --max-jobs 1 \
  --out-link /home/inanna/.cache/tidepool/codex-affinity-check
```

Build output is in `/tmp/tidepool-codex-affinity-nix-serial.log`. Do not restart
the active build just because that log is buffered. After success, protect the
package output with its own GC root and run both namespace smokes from the
Codex checkout against the packaged binary:

```sh
python3 scripts/test-destination-fork.py /absolute/path/to/packaged/codex
python3 scripts/test-destination-fork.py /absolute/path/to/packaged/codex --code-mode
```

These packaging checks remain outstanding. Keep the previous Nix output link,
retained roots, diagnostic traces, and worktrees until their owners deliberately
retire them. Successful canary children have already completed typed cleanup.

### Review and next campaign

The closing review checked transport versus execution identity, durable cache
affinity selection, and direct tool exposure. No additional blocker was found
in those reviewed paths; this is not an exhaustive concurrency audit. The
failed-admission recovery gap below remains a concrete follow-up.

After packaging, use a small real coding task to exercise a medium-effort root,
low-effort child, retained follow-up, and Git integration. Keep campaign types
and helpers authored in the session. Do not turn the diagnostic scripts into a
new permanent DSL before that experience establishes a useful common surface.

### Semantic acceptance

Verify the final implementation with no diagnostic switch, a fresh Astra medium
root and low child, and first-inference usage. Regression tests must cover equal
routing identity in HTTP headers, WebSocket handshake headers, and request
metadata, while asserting independent runtime identities. Recursive forks and
resumes must preserve the entire affinity record.

The provider's exact token-cache placement remains opaque. These measurements
establish a useful routing fix, not a guarantee that every inherited token hits:
new generated tails and the provider's incremental versus shared cache behavior
can leave uncached input even with correct affinity.

For the model-facing surface, retain the first provider usage observation for a
fork separately from the last response. A cache acceptance test must inspect the
first child inference and compare it with the eligible inherited prefix; later
child hits cannot satisfy that criterion. Cache hits are provider observations,
not unconditional guarantees of a successful context fork.

Separate follow-up: failed unfold admission left diagnostic actors/worktrees
observable while returning no typed fork-group handle. The model could inspect
the failure but could not use `planCleanup` on that group. Review admission
rollback or typed recovery evidence at its existing owner rather than exposing
handle constructors or teaching models to reconstruct identities.
