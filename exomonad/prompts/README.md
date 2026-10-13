# Prompt artifacts

This tree contains stable, Tidepool-authored model instructions compiled into
their owning Rust targets with `include_str!`. It is not a runtime
configuration directory: there is no prompt loader, template syntax, or
fallback copy.

| Artifact | Owner | Purpose |
|---|---|---|
| `base.md` | `tidepool::actor_host` | Shared Exomonad model instructions |
| `api-guide.md` | `tidepool::actor_host` | Shared core API reference appended to the base |
| `agent.md` | `tidepool::actor_host` | One task-neutral developer instruction for every actor |
| `haskell-tool-instructions.md` | `exomonad-actor::resident_interactive` | Hosted-tool usage instructions |

Hosted tool descriptions are owned by their declarations: `status` and `reload`
in `exomonad/actor/src/*_tool.rs`; notebooks, commands and lookup in
`bridge/haskell/lib/Tidepool/{Agent/Contract,Command/Tools,Lookup/Tools}.hs`.
The host preserves the AgentSpec's typed declarations, including descriptions;
editing a separate Markdown copy would not change those tools. Describe when to
choose a tool, the evidence its result supplies, and the next decision after
partial success or refusal. Keep per-tool descriptions within the provider's
1,024-character limit; schemas own field shapes and detailed references own
long examples.

User tasks, Haskell-authored startup values, and operator input are authored
content, not stable prompt artifacts.

## Inventory boundary

- **Artifact:** the files listed above are stable instruction blocks. Their
  owning catalogs enumerate and compile them; `agent.md` has no actor-role
  variants or runtime state.
- **Typed dynamic rendering:** actor goal and lifecycle notices, workbench
  receipts, typed-hole cards, effect-row help, corrective turns, round-budget
  notices, and compaction requests combine closed runtime state with guidance.
  Keep them in their typed Rust renderers; no renderer recovers state by
  matching rendered text.
- **Diagnostic:** compiler/workbench/provider failures, setup remediation,
  command errors, and operator-facing status remain with the mechanism that
  detects the condition.
- **Authored task data:** initial messages, completion tasks, harness prompts,
  requests, startup values, and operator messages remain content supplied by
  Haskell or the operator.

Activation messages carry request previews and reply declarations when
available. Current authority and workspace are runtime facts shown by the launch
surface and `status`. Stable prompts teach composition and conditional discovery
without duplicating those facts. Hosted requests settle through `respond`; an
interaction with no active request has no reply binding. `exomonad/docs/`
supplies on-demand examples, including executable examples covered by
actor-host checks.

## Prompt selection

The host composes `base.md` followed by `api-guide.md` as the base selection,
plus `agent.md` and host-owned invariant/tool guidance. Workspace `base`, `agent`
and `[prompts].api_guide` overrides are relative to `.exomonad`. Without a profile
guide the shared API guide is selected; it remains available through lookup
`doc api-guide`. Native multi-agent tooling remains disabled.

An actor generation captures prompt bytes together with source and compatible
tool implementations. Explicit `reload` publishes edited workspace prompts with
those components. Prompt-only changes count as changed configuration and reset
the notebook. Already-issued requests, retries and existing children retain their
captured generation. Compaction uses the same generation selection rule.
Fresh-context children select the published source.
Conversation history is not rewritten to activate instructions. Host-owned
invariants, external service configuration and package pins remain fixed inputs.

The composed fingerprint records selected prompt bodies and hosted-tool guidance;
individual per-tool declarations are outside it. It does not certify provider
application or cache reuse. Inspect actual normalized
provider requests to compare complete instructions and tool surfaces. Shared prompt
source changes still need a matching host build; workspace overrides activate
through coherent reload in that host.

The shared base centers expressive Haskell composition: functions, optics, local
types, typed agent RPC, and small languages interpreted by actors with Jev in
their handlers. It teaches task acceptance, structural recognition, method
selection, and evidence. `exomonad-project-work` is optional Git delivery policy;
general exploration and actor programming remain compositional. Keep core
signatures and compact examples in the shared API guide and longer references in
on-demand documents and skills. Prompt size is an observation, not a fixed
ceiling. Editorial walkthroughs are design hypotheses; the proposed
[behavioral measurements](prompt-measurements.md) need actual execution to
establish efficacy.
