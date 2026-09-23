# Prompt artifacts

This tree contains stable, Tidepool-authored model instructions compiled into
their owning Rust targets with `include_str!`. It is not a runtime
configuration directory: there is no prompt loader, template syntax,
environment override, or fallback copy.

| Artifact | Owner | Consuming role |
|---|---|---|
| `base.md` | `tidepool::actor_host` | Shared Exomonad base instructions, replacing the backend default |
| `api-guide.md` | `tidepool::actor_host` | One shared core API guide, appended to the base for every role |
| `scaffolding-agent.md` | `tidepool::actor_host` | Scaffold-focused coding actor instructions |
| `integration-agent.md` | `tidepool::actor_host` | Integration actor instructions |
| `root.md` | `tidepool::actor_host` | Developer instructions for the root actor |
| `recreated-root.md` | `tidepool::actor_host` | Developer-instruction suffix for a retained conversation on a new actor incarnation |
| `worktree-agent.md` | `tidepool::actor_host` | Instructions for a worktree-backed child actor |
| `readonly-agent.md` | `tidepool::actor_host` | Instructions for a child actor without a coding worktree |
| `haskell-tool-description.md` | `exomonad-actor::resident_interactive` | Hosted-tool description |
| `haskell-tool-instructions.md` | `exomonad-actor::resident_interactive` | Hosted-tool usage instructions |

`harness/system-framing.md` is consumed by the retained harness prompt catalog;
`selfharness/memory-curator.md` is seeded as the memory repository's `AGENTS.md`.
User tasks, Haskell-authored startup values, and operator input are authored
content, not stable prompt artifacts.

## Inventory boundary

- **Artifact:** the files listed above are stable instruction blocks with fixed
  roles and no runtime facts. Their owning catalogs enumerate and compile them.
- **Typed dynamic rendering:** actor goal and lifecycle notices, workbench
  receipts, typed-hole cards, effect-row help, corrective turns, round-budget
  notices, and compaction requests combine closed runtime state with guidance.
  They remain in their typed Rust renderers; no renderer recovers state by
  matching rendered text.
- **Diagnostic:** compiler/workbench/provider failures, setup remediation,
  command errors, and operator-facing status remain with the mechanism that
  detects the condition.
- **Authored task data:** initial messages, completion tasks, harness prompts,
  fork briefs, startup values, and operator messages remain User content
  supplied by Haskell or the operator.

Activation messages are rendered by Rust and carry request previews and reply
declarations when available. Authority and assigned workspace appear once in
per-incarnation launch instructions; detailed state remains in `status`. Stable
prompts teach composition and conditional discovery without duplicating runtime
facts. Hosted requests settle through `respond`; roots outside a request have
no reply binding. `exomonad/docs/` supplies on-demand `:doc` examples, including
executable examples covered by actor-host tests.

## Exomonad base selection

The host compiles `base.md` followed by `api-guide.md` into one base artifact in
its prompt catalog. The API guide is the same superset for every role: it
contains no per-actor inventory or role-dependent substitutions. Runtime
authority still governs which operations an actor may use. At startup, the host
materializes the exact bytes once beneath the run's `prompts/` directory, using
the content hash as the filename and rejecting mismatched existing artifacts.
Every fresh, resumed, or forked launch receives the same absolute file path
with a read-only directory overlay. The Codex adapter supplies
`model_instructions_file` explicitly, which takes precedence over
operator/project defaults and inherited base instructions. Role instructions
and runtime facts remain a separate `developer_instructions` layer; native
multi-agent tooling remains disabled.

The composed prompt fingerprint covers base instructions, role instructions,
and the hosted-tool fingerprint. Source edits take effect after rebuilding and
starting a new host; they do not change the base used by descendants of the
current host. Reattaching an old conversation under a newly built host selects
the new base explicitly and can change its provider prefix. Fingerprints record
what Exomonad selected; they do not certify provider application or cache reuse.

The base adapts the bundled Astra prompt's autonomy, collaboration, and
engineering guidance to Exomonad's scaffold/fork/fold mode. Core signatures and
compact compositions stay in the shared API guide; detailed reference and
longer executable examples live in on-demand documents and skills. Base-prompt
wording is a design choice to exercise through real project work, not a
validated efficacy result.
