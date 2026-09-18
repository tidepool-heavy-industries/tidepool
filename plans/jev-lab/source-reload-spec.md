# Reloading project source in a live session

Astra's specification, given on 2026-09-18 when asked what it would want. Quotes
are Astra's own words. This is a requirements document, not a design.

The point, in Astra's words: *"The goal is not to make notebooks empty. It's to
make a cell read like the work I'm doing now, rather than a repeated
implementation of how to do it."*

Today a model can already write a module under `.shoal/Project/` with ordinary
file tools. What it cannot do is make those definitions available without
restarting the session, so authoring effort compounds across restarts instead of
during the work.

## Semantics

**Transactional reload for future cells; existing values stay attached to their
original code. No implicit rewriting of running work.**

1. **Later cells see new definitions without re-importing.** The import names the
   module; a successful reload advances that module's active revision. Existing
   bindings keep their captured implementation: if `oldHelper = RunAhead.handleFailure`
   was bound before, `oldHelper` keeps the old code while a later mention of
   `RunAhead.handleFailure` gets the new one.
2. **A type change across that line must diagnose itself.** Mixing an old value
   with a new function may fail to typecheck. The diagnostic *"should identify the
   two revisions — not present two identically named types as an inexplicable
   mismatch."*
3. **Reload rebuilds the reverse-dependency closure.** If `CommandEvidence`
   changes, rebuild `RunAhead` and every other loaded project module importing it.
   Publish the new graph atomically only if the whole affected set compiles. *"Do
   not silently leave active RunAhead linked against old CommandEvidence while
   advertising the project as reloaded."*
4. **Default unit is every changed source across the configured roots, as one
   transaction.** Three cooperating files edited together are checked together. A
   targeted single-module option may exist, but must still rebuild affected
   dependents and explain its scope. *"No surprising partial activation."*

## Failure

A failed typecheck is *"an expected result a program can handle, not a catastrophe
that hides earlier work."*

- Edited files stay untouched on disk.
- The previously compiled graph stays active.
- The receipt says plainly: reload failed; edited source remains on disk; the
  active compiled revision is unchanged. It carries the diagnostics and names the
  source snapshot that failed.

## Revision identity

Content-based, over source and the relevant compilation inputs. A short increasing
generation number is fine for display but is not identity. Modification time is
not identity. A clean git commit must not be required for an experiment.

Three things a program compares:

- the source snapshot a handler was compiled against;
- the currently active snapshot;
- the latest observed disk snapshot.

## Provenance

Authoritative provenance as **ordinary data**, with the status view rendering the
same information. Not a field the model must remember to put in actor state. For a
collector, Astra wants to inspect the code revision its current handler was built
from, the relevant loaded module revisions, whether a newer revision is active in
the notebook, and the revision used by each recorded invocation, especially across
`replace`.

That supports programs of the form *"this collector is still on the previous
implementation; prepare a replacement"* without parsing status text. A
human-authored label may supplement provenance but must not establish it.

**Honesty limit, Astra's own:** *"don't claim to identify every implementation
reachable through arbitrary captured functions unless the runtime really tracks
that. 'Compiled against this source snapshot' is a useful, honest starting
point."*

## Where source may come from

The workspace's **declared source roots** — not a hardcoded `.shoal/Project`, and
not arbitrary filesystem discovery. A worktree checkout may be a source root when
explicitly selected, under existing worktree authority. Never silently read
another live actor's working files.

Pinned flake inputs stay immutable; `flake.lock` is their pin. To develop one, the
workspace configures an explicit local override, visible in the compilation
receipt. *"I do not want an apparently pinned dependency to change because I
edited its cached files."* The existing `[haskell.flake_overrides]` mechanism is
that route.

## Surface

A **Haskell-callable operation returning a typed reload result**, so a program can
consume compilation failures, gather evidence and decide its next prepared action.
A convenience tool may exist but must not be the only route.

It is a compilation boundary, not dynamic scope, and the distinction must be
explicit so a model does not expect to reload an unknown module and call its new
API in the cell that requested the reload:

- the cell requesting the reload was compiled against the old environment;
- its remaining computation keeps that environment;
- newly loaded definitions become available to subsequent cells.

## The cells Astra wants to type

Desired shape, not an API proposal. Astra: *"I care more about those semantics and
useful receipts than these names."*

```haskell
reload <- Source.reloadChanged
reload
```

then, in a later cell:

```haskell
import qualified Project.RunAhead as RunAhead

result <- RunAhead.handleFailure task evidence
```

and for provenance:

```haskell
running <- Source.provenance collector
active  <- Source.activeRevision "Project.RunAhead"
(running, active)
```

## Promotion from discoveries to Project

`.shoal/discoveries/` holds experimental source, examples and observations;
`.shoal/Project/` holds modules two consumers genuinely share. Discoveries must
not become a competing library location. Inherited live bindings are snapshots,
never the distribution mechanism.

Astra moves a file across that line when there is a concrete second use, its
inputs and unresolved outcomes are understandable without the transcript, it has a
runnable example and a preserved revealing failure, and it is not merely a saved
task-specific transcript.

*"Promotion shouldn't create another ceremony. Normal compilation and focused
checks are enough. The notebook should expose dependencies or source mismatches,
not certify that a helper is generally reliable."*

## What this is expected to move out of cells

Reusable mechanics: stream capture, contextual selection, read-ahead traversal,
receipt types, recurring event-handler bodies.

What stays in a cell on purpose: today's intent and the selected Reflect context,
concrete candidate actions and source locations, policy choices specific to this
task, handles and intermediate evidence, and a new experimental branch until it
earns reuse.
