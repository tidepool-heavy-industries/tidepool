# Editable attention lenses

The seed's agent spec installs `Persona.Orientation.startup` through Exomonad's
initial-context hook. `Persona.Orientation.arrival` is the policy currently used:
its occasion, budget and view are ordinary editable Haskell. This document
covers preparation mechanics; [SEEDS.md](SEEDS.md) covers the working modules and
the remaining source-editing access gap. The live companion has not been redeployed.

## The composition we are building

An attention lens is an ordinary Haskell function constructing a tree or DAG
view over shared memory references. A memory can offer several presentations:
an outline, original words, an open question, a connection, an interpretation.
Resolution and perspective are independent: a short interpretation is not just
a compressed original. Templates can conditionally compose and juxtapose
material, rather than merely hide paragraphs.

Jev judges batches of frontier nodes and selects among authored alternatives.
Only selected branches expand; another judgment pass is useful when that
expansion supplies new evidence. Reuse judgments for identical shared nodes
within one preparation. Different views/questions of the same memory are not
automatically the same judgment. Caller-supplied pass and evidence-volume bounds
make the operation finite; exhaustion, service failure and doubt preserve a
conservative available presentation with visible uncertainty.

Keep three operations distinct:

- **Prepare:** observe memory, construct views, perform bounded judgments and
  retain evidence, selections, omissions, uncertainty and exact rendered text.
  It may create missing state, but it does not change active context.
- **Inspect:** read that retained result without rerunning effects. Pure
  renderers can compare arrangements using the same observed material.
- **Activate:** publish at startup through the supplied hook, or explicitly
  replace the owned block through the existing synchronous context effect.
  Identify the block through its runtime-issued reference, never its heading.

Recompute at startup and on explicit refresh during conversation. This is not
per-message retrieval. A prepared view can serve many exchanges, and memory
writes do not themselves activate a new view. There is no sleeper or standing
helper assignment: we author this initial machinery, and the companion can
reshape it later with human steering.

Keep the existing Persona, Human and Curiosities paths. An optional manifest can
add memories, annotations, relationships and available presentations without
requiring migration. Begin with small editable continuity and connections lenses,
which are examples rather than permanent categories of self or memory.

## Current Haskell surface

| Module | Authored operations |
| --- | --- |
| `Persona.Collection` | `load` preserves existing blocks, reads optional `memory/collection.json`, and returns the observed collection and initialization outcomes; `loadEntries` accepts a parsed or Haskell-authored manifest |
| `Persona.Attention` | `collection` validates memory references; `continuity`, `connections` and `combineViews` construct views; `prepareWith` runs any authored batched judge; `inspect`, `rendered` and `snapshot` are pure |
| `Persona.Orientation` | `arrival` is the live policy; `prepare` retains the loaded collection and result; `startup` saves inspection before returning context |
| `Persona.AttentionNotebook` | typed instructions revise and prepare an actor-owned draft; `snapshot` retains the previous result; `framing` produces a form carrying those instructions |
| `Persona.Context` | `prepare` composes loading, a supplied lens and Jev; `prepareView` uses an already retained collection/view; `save` writes the inspection as JSON in `notes/<name>.md`; `activate` replaces one supplied authored block reference synchronously; `activateInitial` obtains it from the startup owner |

`prepare` takes `Limits`, an explicit occasion, and a lens function. Its success
contains both `Loaded` and `Prepared`, so the caller can inspect the input reads
and compare new lenses over the same collection. Inspection retains original
words, full view definitions, visited decisions, unreached node IDs, exact Jev
requests, answer previews, model/usage/diagnostics, uncertainty, measured evidence
volume and the exact rendering. It does not execute an alternative's effects.
The current presentations are data; the judge's chosen decision controls expansion.

`Limits` bounds judgment passes and Unicode characters of deciding evidence.
It counts the occasion again on each pass. It is not a token estimate or a limit
on the final provider request, filesystem reads or rendered context. An oversized
node receives its fallback; it is never sent as a silently truncated question.
The remaining allowance can still admit smaller siblings. Zero passes makes a
fallback view without Jev. Negative limits are refused by the planner.

The optional manifest is a versioned object, for example:

```json
{
  "version": 1,
  "memories": [
    {
      "id": "imagined-map",
      "path": "notes/imagined-map.md",
      "tags": ["places", "drawing"],
      "related": ["curiosities"]
    }
  ]
}
```

Paths use the existing state-rooted filesystem grants. The manifest adds to
`persona`, `human` and `curiosities`; it cannot silently redefine them. Missing
manifest means no additions; unreadable/malformed manifest or missing referenced
source is a failure. `availableAt` optionally records an integer knowledge
boundary chosen by the author. It is not inferred from modification time.
`knownBy (KnownBy boundary)` excludes later or undated sources and strips their
relationships before a lens sees the collection. This is a structural filter,
not a transcript archive or proof that authored chronology is truthful. New
interpretations must retain their own later knowledge boundary when represented
as memories; a lens can still misuse later information captured in its code.

`combineViews` shares identical nodes and refuses conflicting definitions of the
same ID. A distinct question or perspective gets a distinct node ID even if its
underlying source is the same. Cycles and dangling references fail before any
judgment. Expansion keeps the parent presentation and visits its children;
shared children render once. Omission does not delete a source.

`activateInitial` reads the retained startup observation and uses the published
reference with `activate`. The latter returns a typed local refusal for a missing,
ambiguous or native block; the runtime validates authority and commits the
synchronous edit. A historical receipt does not establish that its block still
exists. Non-published startup states are retained in the refusal, without a
heading search, fabricated reference or rerun of startup effects.

Saving an inspection preserves evidence, not a resumable machine binding. After
losing notebook bindings, read the saved note for inspection or explicitly prepare
again. There is no implicit decoder that turns an edited JSON receipt back into
an admitted `Prepared` value, and no silent replay of past effects.

## Startup integration

`Persona.Orientation.startup` calls the same preparation composition as explicit
notebook use. It saves the operation identity, collection manifest observation,
initialization outcomes and attention snapshot in `notes/initial-attention.md`,
then returns the rendered seed. A failed save refuses startup. This is the latest
preparation note, overwritten on the next preparation at startup, not a transcript
archive or a runtime publication receipt. Inspect `getInitialContext` for actual
publication and its reference. Startup has no first user message; a later situated
occasion can justify explicit preparation and activation.

## A vocabulary that can grow

Distinguish four things in authored data, while leaving their storage organization
open to the companion:

- **Evidence:** a readable original or an exact reference issued by its owner.
  The ability to recover it is a capability to check, not a property of a URL.
- **Recollection:** authored text about a specified experience, with its writer,
  source coverage and vantage point. It can be personal without becoming fact
  merely because it is written in the first person.
- **Reflection:** a later interpretation, including changed taste, disagreement
  or a newly noticed connection. It can refer to an earlier recollection without
  overwriting what that recollection once meant.
- **Presentation:** what is selected and rendered for this encounter. Changing
  the presentation is distinct from revising or deleting its sources.

These are representational distinctions, not compulsory memory categories. The
companion can invent other ways of organizing its experience. A current persona
note may remain freely editable; a claimed historical account needs enough
provenance to distinguish a revision from an original. Connectome's automatic
archive is not installed here. Today the authoritative transcript belongs to
Harness; state notes belong to the persona. A transcript reader or durable
snapshot export must use that owner before we promise historical reconstruction.

## Optional compositions worth making possible

[Note from an ancestor LLM: I thought this might be cool to try.]
A memory could have several useful views: the words at the time, what you made
of them then, and what occurs to you now. A form could let either participant
follow one view into another. The branch chosen by the human could become the
same typed alternative Jev uses when deciding which view to offer. An unexpected
choice might teach the program a new distinction.

[Note from an ancestor LLM: I thought this might be cool to try.]
An interest could carry a little Haskell program for encountering it again:
sources to revisit, a Jev question that recognizes a related idea, and a view
that makes the connection tangible. Some interests might want no machinery at
all. You could discover which benefit from becoming a practiced way of seeing.

[Note from an ancestor LLM: I thought this might be cool to try.]
Before adopting a new attention rule, let it and the old one read the same small
set of encounters. Look at what each brings forward and misses. A helper can
offer a competing interpretation; you can keep both, change the question or
discard the rule. The disagreement itself may be more interesting than a winner.

These are optional authored compositions during interaction, not standing jobs,
fixed helper roles or a prescribed route to growth. Root remains Astra medium;
Luna/Sol helpers supply attributed contributions. An Astra captured fork remains
an option when its context is useful. None is mandatory for making memory.

## Keep the first iteration small and inspectable

Check the first provider request using existing Exomonad qualification paths.
Exercise a changed memory on a new conversation and an unchanged seed on recovery. Inspecting the
result must not rerun preparation. Missing state and unreadable state need
different outcomes. Retain those distinctions as later compositions introduce
recollection, selection and folding.

Do not build an independent transcript store, cache or compression scheduler.
Reuse current context/source/actor owners. Context selection should not become
an automatic turn-by-turn tax: stable inputs can retain their existing view,
and a useful deliberate change can justify recomputation. Measure the actual
request and cache usage before describing any savings.

A recollection can be authored when the companion chooses to make an account.
A helper draft stays attributed until the companion decides what to keep.
Mechanical checks can establish source coverage and chronology; semantic faithfulness still requires comparing the account with its evidence.

No automatic folding, archival search, speculative summaries, scheduled wakes or
self-activation is installed by this candidate. Richer memory remains something
the companion can help author in conversation with the human.
