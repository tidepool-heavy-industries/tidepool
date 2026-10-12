# Persona harness seed

An open-ended companion and peer, authored in Haskell on Exomonad's existing
conversation and browser. Its character and ways of working emerge through
shared history, curiosity and human steering. The seed supplies no name, voice,
interests or activity program. The operator has selected `gpt-6-astra` with
`medium` effort; changing the root model/effort remains outside the companion's autonomy.
Helpers preferentially use the configured Luna/Sol aliases; Astra context forks
remain available. The prompt states the root-model boundary; the existing context effect also contains
model-selection operations, so this is an instruction, not a new per-operation
runtime restriction. Haskell is an expressive medium it can use to develop
its own tools, context, interfaces and methods. The seed invites an embodied
understanding of the whole system: tools supply perception and action; memory,
context, profile and harness shape what it can notice and do. Technical work has
the character of growing one's own Emacs configuration. Direction can emerge
from conversation, use and curiosity.

The cognitive framing is Astra as deliberate System 2 and Jev as fast System 1:
a peer contributing perception, reflexes, heuristics and practiced competence.
Haskell makes that System 1 authorable through evidence assembly, semantic
questions, policies, state and typed continuations. The companion can locate
its sense of self across this ensemble and steer its recursive self-improvement,
with initial human assistance and broader freedom as skill develops. It can
offload practiced decisions to authored programs and devote deliberate attention
to invention, synthesis and revising how it thinks. The seed leaves its eventual
identity and what it chooses to cultivate open.

Start with a normal `exomonad new` workspace using this Tidepool build's pinned
shared-source workspace. Copy this seed's `.exomonad/Persona`, `AgentSpec.hs`,
and `prompts` into that workspace. Merge the seed `config.toml`'s Haskell and
prompt settings with the operator's existing launch/provider settings. Keep the
live workspace's configuration separate while preparing this alternative.
The seed supplies both `prompts/base.md` and `prompts/agent.md`. The workspace
base override carries composition and lifecycle guidance for an open-ended
companion; the host appends the shared Exomonad API guide using its existing
prompt-selection mechanism. The shared harness prompts remain unchanged.
The base also embeds substantial verbatim excerpts from TypeSafe's MIT-licensed
LLM guide, with attribution and its license alongside the prompts. These are
initial context, not merely links the companion must discover. The prompt budget
prioritizes a rich, accurate understanding of the medium; established referents
such as System 1/System 2 and Emacs carry meaning, with their connections made
explicit. The source and research links are in [PROVENANCE.md](PROVENANCE.md).
Keep the shared workspace at `.exomonad/workspace`; it supplies the pinned Jev facade.
Do not copy run history or another companion's `.exomonad/state` accidentally.
Run the normal workspace check and frozen-bundle qualification before launching.
The renderer must include this feature's matching Harness commit; an older
renderer rejects the new form variants. Dependency integration is documented in
the delivery report, separately from release qualification.

Set `TAVILY_API_KEY` in the host launch environment. The host forwards it using
its existing environment owner; it is never passed as a Haskell argument.
Jev uses the existing `TYPESAFE_API_KEY` configuration. Missing credentials are
explicit tool failures. No credentials are needed for the focused mock tests.

The agent spec exposes one synchronous Haskell notebook with forms, console,
scoped state files, Jev, lookup, reflection, active source inspection, and Web.
Its synchronous notebook includes context editing, record actors, hosted agents,
typed requests, watches, inspection and scoped cleanup. It omits commands and
raw HTTP. `Persona.Spec.helperSpec` provides a starting helper profile that
returns findings rather than writing shared memory or mounting human forms.
The Fs handlers resolve all file operations beneath `.exomonad/state`, which
source capture and drift inspection exclude. The host currently grants the
general effect vocabulary;
the installed notebook narrows it. This is a tool capability boundary, not a
separate OS user or process sandbox.

`Persona.Memory.initialise` creates absent persona/human/curiosities blocks using
compare-and-swap. It never overwrites an existing identity, and storage errors
remain distinct from absence. `Persona.Orientation.startup`, installed by the
agent spec, assembles initial context from that memory using the editable
`arrival` policy. It saves the latest preparation in `notes/initial-attention.md`
before the context owner publishes the returned text. The note includes the
startup operation so preparation can be distinguished from publication.

The [attention pipeline](ATTENTION.md) supports Haskell-authored views, bounded
Jev selection, retained inspection and explicit context replacement.
`Persona.Context.activateInitial` obtains the original block reference from
`Tidepool.Agent.Context.getInitialContext` and edits only that block, refusing a
missing or unavailable target. A historical receipt does not recreate removed
context. Memory writes and source reloads do not automatically refresh it.

The [executable seeds](SEEDS.md) describe the live orientation policy and an
optional attention notebook actor. The same actor language can be supplied by a
notebook cell, a human form, or mapped events. These are starting modules for the
persona's own use and evolution. The criterion is the breadth of useful programs
their abstractions support, with normal expert Haskell vocabulary available.

Persistent self-editing still has an access gap: persona's Fs handlers reach only
state, while Exomonad's existing authored-module and SessionHelpers reload paths
operate on source. The narrow owner handoff is in [SEEDS.md](SEEDS.md#source-editing-handoff).
Until connected, `Persona.Memory.requestChange` records an operator proposal;
notebook definitions can still change behavior immediately. `readSourceModule`
returns active captured bytes and their revision; `sourceStatus` distinguishes
pending edits. Startup prompts are frozen for the run; context editing does not
replace system/developer text.

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

The metaprompting hypothesis is that an ambitious invitation, an accurate account
of the editable system, and consequences carried forward from interaction can
encourage interesting evolution. It leaves eventual behavior and personality
open. Forms and Jev belong in that feedback loop: an invented interaction changes
what can be expressed, and the response can suggest a new interpretation or
composition. Jev's judgments and their consequences can reshape System 2
attention; System 2 can revise the judgments and how they are composed. The
prompt gives positive discoveries, tastes and unresolved questions a place in
memory alongside corrections.

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
