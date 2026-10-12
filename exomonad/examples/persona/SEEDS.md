# Executable seeds

The persona starts with useful authored modules, some already participating in
its experience. Their later shape should follow its own use and human steering.
Established Haskell abstractions are shared vocabulary. Explanations concentrate
on the local execution, publication and lifetime contracts.

Judge a proposed abstraction by the family of useful programs it expresses and
its composition with other pieces. A short implementation of one fixed behavior
does not establish that breadth. These seeds give the persona concrete material
to use, revise and generalize, without requiring a particular personality or
practice of self-improvement.

## Orientation: already in use

[`Persona.Orientation`](.exomonad/Persona/Orientation.hs) contains `arrival`, an
ordinary record of occasion, limits and an `attentionView` function from a
collection to a view.
`Persona.Spec.agentSpec` installs its `startup` function. Fresh conversation
startup loads memory, prepares the view, saves its inspection and returns the
context text. The preparation uses existing file and Jev effects; the initializer
can be edited to compose other effects admitted by the startup owner.

`prepare` is also usable explicitly with another policy. Functions can capture
signals, compose templates, or apply optics to structured data. The present
continuity view is a starting policy. No framework extension is needed to author
another function of the same type.

`notes/initial-attention.md` retains the latest startup preparation: operation,
manifest observation, initialization outcomes, original evidence, judgments and
rendered text. Reading it does not rerun Jev. It is overwritten at the next
startup preparation and is not a resumable machine value. Its existence alone
does not establish successful publication. `getInitialContext` observes that
owner-issued outcome; `Persona.Context.activateInitial` uses its reference for
an explicit synchronous edit. Startup is not rerun by source or spec reload.

## Attention notebook: a small language with a resident interpreter

[`Persona.AttentionNotebook`](.exomonad/Persona/AttentionNotebook.hs) supplies
`Sources`, `View`, `Occasion`, `Budget` and `Prepare` instructions. A program is
an ordinary list. `View` carries a real Haskell function. Several edits can
precede one preparation, and the same program can be produced by a notebook
cell, a form or an event handler.

`execute` is the common interpreter, using ordinary state operations and Jev.
`definition` hosts it in Exomonad's existing record actor; the runtime owns the
mailbox, serialized handlers and actor lifetime. This seed introduces no new
scheduler. It retains the source collection, editable policy and last preparation
attempt. `snapshot` materializes the current draft and separately exposes that
last result, including whether the draft changed since the attempt. An invalid
view or budget is a typed result. A Jev failure preserves the planner's existing
fallback and evidence.

A typical use is to load a collection, construct a notebook with `arrival` or an
edited policy, then start `definition` with `R.start`. With `mempty` as its event
source, work runs only through its typed `run` endpoint. The `inspect` endpoint
reads the retained snapshot without performing judgments. The compiled
`actorExample` in the focused test module checks that concrete start/client/call/
finish composition; that check is not a hosted runtime execution.

`framing` is a form returning the same instruction list. The caller handles
`Submitted`, `Dismissed` and `FormUnavailable`; submitting is distinct from
running the commands. More elaborate forms can select functions with `F.option`,
return several edits, or conditionally construct an instruction list. No string
labels dispatch behavior in this seed.

For ongoing work, map an existing `EventSource` into lists of instructions and
supply it to `definition`. The interpreter can then prepare a new view on an
authored event without another root-model round to orchestrate the transition.
Attaching a helper's settlement still requires the existing request authority;
a handle or label does not transfer ownership. Only include `Prepare` when the
event warrants a new judgment. The actor cannot edit the root's context; its
owner inspects a result and activates it synchronously when desired.

The language covers a family of context preparations: changes in source
selection, perspective, template, occasion and semantic budget. Extending it to
a different family is something the persona can decide through use. Its source
also serves as a concrete pattern for other small languages with Haskell
functions as payloads and typed event-driven interpreters.

No attention actor is launched at startup and no standing activity is installed.
Finish owned actors with `R.finish` and inspect their outcome when their work is
done. Retained actor state survives the launching invocation; source persistence
and saving an inspection do not promise restoration of arbitrary closures after
loss of the machine session. Existing captured functions keep their definitions
after a source reload. Behavioral replacement uses the existing actor API and
returns a new incarnation handle; old endpoints still name the old incarnation.

## Source-editing handoff

The reviewed source is based on integration checkpoint
`6c6c3d4d72d6d7719432b49df8c3abd7dd437f6f`. Exomonad already supports captured
workspace modules, actor-local `.exomonad/helpers/SessionHelpers.hs` and
`SessionHelpers.*`, `reload_helpers`, and `reload_agent_spec`. The persona's
current `FsRead`/`FsWrite` handlers resolve only under `.exomonad/state`.
Consequently it can invent notebook definitions and persist memory, but cannot
yet save those inventions directly into the source locations these owners load.

Request to the Exomonad owner: connect persona's admitted source authorship to
those existing locations and publication mechanisms. Keep the current memory
paths working, expose the exact authored source and active revision, and return
the existing publication/refusal receipt after checking. Cover creating a helper
module, revising a seed, refusing an invalid draft while retaining the active
revision, and keeping already captured values unchanged. This is a capability
and integration dependency; the persona change does not modify runtime source
ownership, transport, grants, or Harness pins.

Until connected, `readSourceModule` and `sourceStatus` support inspection,
notebook definitions support immediate invention, and `Memory.requestChange`
records concrete operator-assisted edits. The prompt names this limit explicitly.

## Verification boundary

The focused persona suite executes the instruction interpreter, typed form
decoding into that interpreter, preparation, evidence construction, the write
effect (including refusal), and refusal
when startup has no published context block. It compiles the actual agent spec
and actor composition. JSON encoding is a runtime intrinsic; its plain-GHC stub
does not qualify the persisted JSON bytes. The suite does not establish a live
actor run, provider relevance, source self-editing, or the experience of the selected frontier model. Those
claims require the joined hosted path and actual human interaction.
