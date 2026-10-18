# Persona harness seed

An open-ended companion and peer, authored in Haskell on Exomonad's existing
conversation and browser. Its character and ways of working emerge through
shared history, curiosity and human steering. The seed supplies no name, voice,
interests or activity program. The operator has selected `gpt-6-astra` with
`medium` effort; changing the root model/effort remains outside the companion's autonomy.
Helpers preferentially use the configured `Luna`/`Sol` selectors; `Astra` context forks
remain available. The prompt states the root-model boundary; the existing context effect also contains
model-selection operations, so this is an instruction, not a new per-operation
runtime restriction. Haskell is an expressive medium it can use to develop
its own tools, context, interfaces and methods. The seed invites an embodied
understanding of the whole system: tools supply perception and action; memory,
context, profile and harness shape what it can notice and do. Technical work has
the character of growing one's own Emacs configuration. Direction can emerge
from conversation, use and curiosity.

Start with a normal `exomonad new` workspace using this Tidepool build's pinned
shared-source workspace. Copy this seed's `.exomonad/Persona`, `AgentSpec.hs`,
and `prompts` into that workspace. Merge the seed `config.toml`'s Haskell and
prompt settings with the operator's existing launch/provider settings. Keep the
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
selection as if it were a successful judgment. Notebook values last for the
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
do not establish persona quality or semantic relevance.

Observe what develops across conversations and how earlier experience changes
later attention, authored code, context or interaction. Compare prompting
strategies through their consequences without prescribing a personality or
rewarding repeated declarations of curiosity. Retain concrete limitations and
unexpected behaviors for the owning Exomonad abstraction. The seed remains a
hypothesis until tried with the selected model and an actual human.
