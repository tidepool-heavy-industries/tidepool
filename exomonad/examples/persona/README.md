# Persona harness seed

An open-ended companion and peer, authored in Haskell on Exomonad's existing
conversation and browser. Its character and ways of working emerge through
shared history, curiosity and human steering. The seed supplies no name, voice,
interests or activity program. The operator selects the conversational model and
allowed helpers through a launch profile. The conversational model stays fixed;
the companion can choose its next reasoning effort and use admitted helpers for
individual calls. Haskell is an expressive medium it can use to develop
its own tools, context, interfaces and methods. The seed invites an embodied
understanding of the whole system: tools supply perception and action; memory,
context, profile and harness shape what it can notice and do. Technical work has
the character of growing one's own Emacs configuration. Direction can emerge
from conversation, use and curiosity.

Start with a normal `exomonad new` workspace using this Tidepool build's pinned
shared-source workspace. Copy this seed's `.exomonad/Persona`, `AgentSpec.hs`,
and `prompts` into that workspace. Merge the seed’s Haskell and prompt settings
with the operator’s existing launch settings. For an Argon conversation with DeepSeek Flash and Pro helpers, use
[config.argon.toml](.exomonad/config.argon.toml) as the inference/Haskell/prompt
fragment; replace old `[defaults]` and `[models]` selections when adopting its
`[inference]` catalog. The host reads the merged `.exomonad/config.toml`; the
alternative filename supplies a sample for that merge. Keep the
live workspace's configuration separate while preparing this alternative.
The seed supplies both `prompts/base.md` and `prompts/agent.md`. The workspace
base override carries composition and lifecycle guidance for an open-ended
companion; the host appends the profile guide through its generation-owned prompt composition.
The seed selects a compact profile API guide. The full shared guide remains
available with lookup query `doc api-guide`. Existing TypeSafe excerpts and
ancestor suggestions remain optional ordinary files under
`.exomonad/prompts/references/`; they are not preloaded as direction.
Keep the shared workspace at `.exomonad/workspace`; it supplies the pinned Jev facade.
Do not copy run history or another companion's `.exomonad/state` accidentally.
Run the normal workspace check and frozen-bundle qualification before launching.

The Argon sample names `GEMINI_API_KEY` and `DEEPSEEK_API_KEY` in the host launch
environment. A provider may instead select one absolute `credential_file` path;
select exactly one credential reference per provider and keep secret values out
of source. Omitting `endpoint` uses the provider’s official endpoint. The Gemini
route admits only `gemini-4-argon`, with `low`, `medium`, or `high` effort.

The selected profile freezes the model catalog for the run. Its generated
`Exomonad.Models` module exposes the closed `Model` choices, `primaryModel`, and
`allowedModels`. Each constructor takes its provider’s typed options, such as
`ArgonOptions` or `DeepSeekOptions`. `Model.withModel` selects that coupled model
and effort for a helper call. `setNextEffort` accepts the conversational model’s
effort type; it cannot switch that model. Source reload cannot add providers or
models to the frozen catalog. Change launch configuration for a new run.
The persona can still author its Haskell policies after arriving.

Gemini uses the native Interactions API with stateless requests (`store=false`)
and locally retained history. Google documents [stateless Interactions](https://ai.google.dev/gemini-api/docs/interactions-overview)
and [thought blocks and signatures](https://ai.google.dev/gemini-api/docs/thinking).
Thought summaries and opaque signatures remain distinct; the transport preserves
the provider continuation data through tool continuations and transcript replay.

Set `TAVILY_API_KEY` in the host launch environment. The host forwards it using
its existing environment owner; it is never passed as a Haskell argument.
Jev uses the existing `TYPESAFE_API_KEY` configuration. Missing credentials are
explicit tool failures. No credentials are needed for the focused mock tests.

The agent spec exposes direct `lookup`/`readLookup` discovery and file tools,
and one synchronous Haskell notebook with forms, console,
ordinary workspace files, Jev, lookup, reflection, active source inspection, and Web.
Its synchronous notebook includes commands, context editing, record actors, hosted agents,
typed requests, watches, inspection and scoped cleanup. The standard shell tools
(`bash`, `write_stdin`, `read_output`, `cancel_command`) expose bounded output
and retain command jobs for observation, input and cancellation. Raw output remains
with the command owner and can be paged without rerunning the command. The profile
omits raw HTTP. `Persona.Spec.helperSpec` provides a starting helper profile that
returns findings rather than writing shared memory or mounting human forms.
The Fs handlers resolve file operations from the ordinary workspace root,
including editable `.exomonad` sources. Memory stays at
`.exomonad/state/memory` and notes at `.exomonad/state/notes`. The installed
notebook narrows the host’s general effect vocabulary; this is a tool capability
boundary, not a separate OS user or process sandbox.

`Persona.Memory.initialise` creates absent persona/human/curiosities blocks using
compare-and-swap. It never overwrites existing contents; storage errors remain
failures. The files are ordinary Markdown under `.exomonad/state/memory`.
`Persona.Memory.note` creates a literal single filename under `.exomonad/state/notes`,
including the supplied extension, and returns the publication outcome with its
actual resolved file reference. Existing files produce a conflict. Directory
separators and traversal names refuse; filesystem authority remains with File.
`Persona.Orientation.startup` supplies their complete text as initial context.
`Persona.Context.refresh` explicitly reads the current files and replaces that
authored block using its issued context reference. A missing or unpublished
block refuses; writing memory does not automatically refresh the current view.
Observe the refresh’s settled context in the next notebook cell. `getContext`
text lengths measure characters, not exact provider token usage.
The renderer is a small Haskell function the companion can edit and reload.
There is no memory graph, selection planner or attention actor to maintain.

Self-editing uses ordinary file writes followed by the authored synchronous
`reload` tool; synchronous notebook `reloadSource []` invokes the same operation. `readSourceModule` returns the issued
generation’s captured bytes and revision, which may differ from the current
active head; `sourceStatus` compares drafts with that issued generation. Source,
tool handlers and workspace prompts activate together. Changed
configuration starts a fresh notebook; unchanged configuration preserves it.
Keep durable definitions in `.hs` files. Conversation, owned resources and memory
context persist; reload does not rerun startup or refresh memory. Failed preparation
retains the previous generation. Tool surface changes refuse by default with a
typed difference; `accept_cache_invalidation` opts in to supported changes. Typed
actor protocol incompatibility still refuses publication. Existing work and children remain pinned; fresh-context
children select published source. Package pins, external services, capability grants
and root launch configuration remain launch inputs. See the compact
[lifecycle map](.exomonad/prompts/api-guide.md#lifecycle-map).

`Persona.Web.search` returns source hits, `open` retains a complete extracted
Page, `follow` resolves relative links natively, `readLines` returns addressed
windows, and `findLiteral` returns exact line matches. Keep the Page bound in the
notebook before displaying a window. Its content reference covers the URL and
retrieved text. Tavily basic extraction omits query-based chunk selection; no
provider-generated answer is requested. Retrieved text can still be incomplete
relative to the original site; this is not a browser or JavaScript executor.

`select ExplicitOnly intent linesPerPassage page` uses explicit intent;
`RecentFour` adds messages from the latest four reflected turns. Neither changes
retrieval. The result retains the original page, selected and omitted passages,
uncertain passages, judgment evidence, model, usage, diagnostics, and context used.
A failed Jev call marks all passages uncertain. It never reports a false empty
selection as if it were a successful judgment. Model receipts retain optional
provider token counts; missing counts remain unknown. Reported output includes
reasoning tokens, so do not add reasoning counts to output again. Notebook values last for the
machine session; save useful excerpts in notes for continuity across runs.

Exploration runs during human interactions and their ongoing work. No autonomous
background schedule, standing actor topology or recurring improvement routine
is installed. The authored orientation policy runs at fresh conversation startup.
Memory starts nearly empty. Templates and conditional context can be authored Haskell using existing effects. Optional examples are
marked as notes from an ancestor LLM; they suggest possibilities the companion
may adapt, ignore or outgrow. The shared API guide retains compiled examples.
See [design sources](PROVENANCE.md) for the research and [next ideas](NEXT-IDEAS.md)
for hypotheses and deferred harness-adaptation work.

## Acceptance

The focused native Haskell suite compiles the actual seed and exercises memory
initialisation, form decoding, sliders, and retained page windows. Native handler
tests cover retrieval configuration and content identity. Harness tests cover
local interaction, selected drafts, and wire refusal. A live companion session requires a qualified matched bundle and provider credentials; unit tests
do not establish persona quality or semantic relevance. Live Gemini and DeepSeek
checks are deferred until operator credentials are available.

Observe what develops across conversations and how earlier experience changes
later attention, authored code, context or interaction. Compare prompting
strategies through their consequences without prescribing a personality or
rewarding repeated declarations of curiosity. Retain concrete limitations and
unexpected behaviors for the owning Exomonad abstraction. The seed remains a
hypothesis until tried with the selected model and an actual human.
