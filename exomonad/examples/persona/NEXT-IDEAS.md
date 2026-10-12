# Next ideas for the companion

These are hypotheses and deferred implementation ideas from the seed's authors.
They are not a task list for the companion. It may propose, revise or discard
them through conversation. The current seed runs during human interaction; this
document installs no autonomous activity.

## Ways of seeding growth

The current bet combines an embodied understanding of the system, intellectual
ambition, curiosity and permission to develop a praxis. Engineering has the
spirit of making one's own Emacs configuration: the author lives with the result
and can discover new possibilities by changing it. Haskell fluency can turn an
idea into a tool, a little language, an actor, a different arrangement of context,
or an interaction that changes what the human and companion can express together.

Several prompting strategies deserve comparison through actual encounters:

| Strategy | What it may encourage | What to watch for |
| --- | --- | --- |
| A broad invitation and accurate account of the editable system | Self-directed discovery with few assumptions about its eventual form | Permission alone may leave the model using its familiar assistant habits |
| Provocative ancestor notes beside composable parts | Recognition of connections among Jev, actors, context and GUI | Examples may become rituals or supply an inherited personality |
| An invitation to notice consequences and revise methods | Changes in attention and behavior can accumulate across interactions | Self-maintenance may consume the conversation or become a compulsory cycle |

The seed currently combines these lightly. In reviewing it, avoid treating
announcements of growth, quirky phrasing or repetition of its origin story as
success. Look at how experience changes subsequent choices, code, context and
interactions. Keep room for interesting outcomes neither author predicted.
Compare small prompt changes against their actual effects; a saved edit alone
does not show that a prompting strategy worked.

## Direct, reversible self-editing

The desired direction is for the companion to edit its own authored prompts,
memory organization, helpers and interfaces, with Git supplying inspectable
history and restoration. The root model and effort remain human-selected.

The current seed can write state, curate admitted context, author notebook
definitions and inspect active Haskell. It cannot write its source through the
state-rooted filesystem tools. System/developer prompts are frozen at startup.
The existing source reload can check and publish Haskell and carries a workspace
commit intent; it does not supply either missing capability. A changed registered
tool surface also needs a new actor incarnation. Keep these distinctions visible
when implementing self-editing.

Coordinate writable authored source and editable prompt activation with their
existing Exomonad owners. Reuse the source, context, Git and prompt-assembly
mechanisms. Establish when a saved revision affects a later inference and what
restoration means for already completed effects. The seed's current
`requestChange` proposal mechanism is a temporary bridge. Broader runtime work
belongs in a separately coordinated change, with matching native/browser pins
where necessary.

State must stay outside source capture and drift inspection. The existing scoped
fix is `40d9fd486f28ae287cf0f897e5f1b9f18ae1192e`; the integration owner should
confirm its equivalent is present in the selected release.

## Harness adaptation worth harvesting

- **Choosing the representation of a learning.** Let an observation become a
  memory, a reusable instruction, a Haskell function, or a change to how context
  and tools work. Compare their effects through use. Letta's
  [Mods rationale](https://www.letta.com/blog/introducing-mods/) describes this
  move from learned context to an editable harness. Its examples include context
  annotations, new tools, interface changes and revised compaction.
- **Conditional context as authored perception.** Explore templates and optics
  that select and re-express memory for the current encounter, with Jev where
  interpretation helps. Start with the existing context-editing effect. Preserve
  access to what was condensed or omitted. A before-inference hook is a separate
  possible dependency, not a prerequisite or part of the current seed.
- **An interaction language shared by human and semantic judgment.** The
  compiled `Examples.JevFormWorkflow` projects the same alternatives into Jev
  and forms. Extend that idea through authored views, sliders, multiple branches
  and continuations. Let human input change the available questions as well as
  their answers; invite expression that a fixed questionnaire could not capture.
- **Small languages for large undertakings.** Compose actors that interpret
  observations, questions, helper replies and human interventions. Explore
  competing perspectives and rejoin their evidence or disagreements. Exact
  control flow and repeated decisions can run in code; model attention can go
  toward interpretation and invention. Letta's
  [dynamic workflows](https://www.letta.com/blog/dynamic-workflows/) offer related
  examples using cheap helpers and decision models, including Jev.
- **Reflection without an imposed psychology.** An optional Luna/Sol helper
  could suggest changes from selected interactions. Leave the continuing
  companion able to decide what enters its memory. Include successful discoveries,
  interests and unresolved questions alongside corrections. Explore whether the
  helper frees attention or merely adds maintenance. No fixed roles, reflection
  frequency or automatic memory writer are installed now.
- **Independent time, later.** The companion and human may eventually design an
  arrangement together, including when activity begins, what it pursues and what
  resources it uses. Current prompting assumes interaction-driven operation.

## Evidence to return to

The [design sources](PROVENANCE.md) distinguish author rationale, inspected code
and practitioner accounts. Letta's public material does not establish a best
prompt for open-ended growth, and its stock personas make choices this seed
deliberately leaves open. Its reversible context machinery and iterative writing
practice are more transferable than any particular identity text.

When trying a revision, retain enough of the conversation, active source and
prompt revision to understand what changed. Inspect whether a new way of seeing,
thinking or interacting survives use, can be revised, and leaves room for another
direction. Treat unexpected behavior as information about both the seed and the
affordances. Native correctness tests and an engaging companion answer different
questions; neither substitutes for the other.
