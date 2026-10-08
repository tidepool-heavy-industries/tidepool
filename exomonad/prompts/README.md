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

Hosted tool descriptions are owned by their declarations: `status` and reloads
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

The host compiles `base.md` followed by `api-guide.md` into one frozen base
artifact. Every actor receives this same content-addressed file. The host also
uses one task-neutral `agent.md` developer instruction; the active typed request
and runtime status supply the task and authority. Workspace core overrides are
selected once at run startup, never by actor role or live bindings. Native
multi-agent tooling remains disabled.

The composed prompt fingerprint covers the frozen base, the agent instruction,
and shared hosted-tool usage instructions. It does not fingerprint individual
per-tool declarations; inspect actual provider requests to compare the complete
surface. Source edits take effect after rebuilding and starting a new host; they
do not change the content used by an existing host. Reattaching a conversation
under a new host selects its new base explicitly. Fingerprints record what
Exomonad selected; they do not certify provider application or cache reuse.

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
