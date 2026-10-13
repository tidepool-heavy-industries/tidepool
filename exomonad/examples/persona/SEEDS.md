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
refuse. Frozen system/developer prompts remain fixed for the run.

`Persona.Web` retains search hits and complete extracted pages for addressed
inspection, with optional Jev selection. `Persona.Spec.helperSpec` is an
optional read-only helper profile; no helper or standing activity starts at boot.

## Source editing

Files resolve from the ordinary workspace root. `.exomonad/Persona/*.hs` and
`.exomonad/helpers/SessionHelpers.hs` are editable files. Durable memory remains
under `.exomonad/state`, excluded from source capture and drift.

Read a draft with ordinary file access; compare active code through
`readSourceModule` and `sourceStatus`. Write a revision, then explicitly validate
and publish through `reloadSource`, `reload_helpers` or `reload_agent_spec`.
Failed validation retains active source. Already captured values retain their
definitions; a changed tool surface requires a new actor incarnation. Minor
source edits need no separate edit API or coordinator handoff.

See the [quickstart lifecycle map](.exomonad/prompts/api-guide.md#lifecycle-map)
and the exact lookup locators there for deeper notebook, actor and Jev contracts.
Optional predecessor ideas remain under `.exomonad/prompts/references/`.
