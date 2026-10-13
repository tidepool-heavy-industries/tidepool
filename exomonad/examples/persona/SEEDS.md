# Editable companion modules

`Persona.Memory` reads and writes three ordinary Markdown blocks: persona,
human and curiosities, under `.exomonad/state/memory`. Initialisation creates
only absent files. `note` saves named notes under `.exomonad/state/notes`.
Read failures remain failures; existing contents are never reseeded.

`Persona.Orientation.renderMemory` reads those bodies in full. Its `startup` function initialises only absent seed files, then
returns the current bodies through the installed initial-context hook. This small Haskell
composition is the place to author a different arrangement when actual use
suggests one. It has no mandatory semantic planner or memory graph.

`Persona.Context.refresh` explicitly reloads memory and activates it through the
context owner’s original publication reference. Writes do not refresh context.
`activateInitial` can activate authored text; unavailable or unpublished targets
refuse. Workspace prompts activate through explicit reload.

`Persona.Web` retains search hits and complete extracted pages for addressed
inspection, with optional Jev selection. `Persona.Spec.helperSpec` is an
optional read-only helper profile; no helper or standing activity starts at boot.

## Source editing

Files resolve from the ordinary workspace root. `.exomonad/Persona/*.hs` and
`.exomonad/helpers/SessionHelpers.hs` are editable files. Durable memory remains
under `.exomonad/state`, excluded from source capture and drift.

Read a draft with ordinary file access; inspect the issued generation’s captured
code through `readSourceModule` and draft drift through `sourceStatus`. The issued
generation may differ from the current active head. Write a revision, then call native `reload`
or Haskell `reloadSource`. Source, compatible tools and workspace prompts publish
as one generation. Changed configuration starts a fresh notebook; unchanged
configuration preserves bindings. Keep durable definitions in `.hs` files.
Reload preserves conversation, resources and memory context, and does not refresh
memory or rerun startup. Failed preparation retains active configuration.

Already-issued work and existing children retain their generation. Fresh-context
children select published source; captured-only children remain pinned. Incompatible
registered tools or typed actor protocols refuse publication; a changed tool surface
requires a new actor incarnation. Modules can be added, edited or removed inside
declared roots, with no older source fallback. Package pins, external services,
capabilities and root launch configuration remain launch inputs.

See the [quickstart lifecycle map](.exomonad/prompts/api-guide.md#lifecycle-map)
and the exact lookup locators there for deeper notebook, actor and Jev contracts.
Optional predecessor ideas remain under `.exomonad/prompts/references/`.
