# Small model turns inside Haskell

Use ordinary Haskell for deterministic work and Jev for a compact semantic
choice. Use `Tidepool.Model` when a bounded piece of work needs several tool
calls and a generated answer, while the surrounding Haskell program keeps
ownership of the workflow. Use an actor when it needs an ongoing mailbox,
independent lifecycle or delegated implementation.

`Coordination.hs` is the executable source of four seeds: route a change from
actual diff evidence, clarify a dependency with its owner, prepare an evidenced
handoff, and inspect retained failure output. Supply project-specific callbacks;
only those tools are available. Keep important authority in the callback owner.
The examples deliberately expose no shell or rerun operation.

Build a reusable `textTurn model` or `typedTurn model` with an explicit
`Model family`, then call `invokeModel turn input`. The program requires
`Member (ModelCall family) effects`; the runtime checks its exact model and
effort against the actor's admitted permissions.
Tool records, argument/result schemas and optional after-tool hooks are the same
`AgentSpec` surface. This initial adapter accepts JSON function tools with object
inputs; raw custom tools and scalar inputs are refused before provider admission.
Each function tool explicitly selects its text with `presentWith` (for example,
`presentWith id` for `Text` or `presentWith presentJson` for JSON); hooks retain
the semantic result separately from that text.
Use small records for arguments. A typed result may contain nullable fields,
numbers and disjoint tagged alternatives. Unsupported schemas fail preflight.

Match `modelOutcome`, retain `modelReceipt`, and handle a budget failure as an
ordinary result before running cleanup. The invocation receipt contains evidence
references and measured usage; it does not flood the notebook with a transcript.
A hook may prune only the exact retained output handle it receives.

The effect is opt-in and requires the admitted cell service. These seeds are not
advertised in the always-loaded prompts until that engine hook passes joined
acceptance. They have native Haskell
compile coverage; native contract tests execute callbacks in the caller effect
row. This is separate from resident/JIT and live-provider acceptance.

## Editing an agent's retained context

`Tidepool.Agent.Context` reads the current actor's active committed projection
as an authority-free `Context`. `contextBlocks`, `visibleTexts`, and
`referencedTexts` inspect its native structure, full text, selectors and origins.
`editableTexts` is a read-only fold. Native Haskell source/input and function
arguments stay pinned. `display` gives bounded text previews, character counts,
exact references, selectors and content versions. `contextAuthorship` reads
the immutable canonical original occurrence's role/type and authored-note flag,
keyed by reference and selector. Unknown remains explicit. This metadata does
not attribute replacement text to the original author; the later editing
transaction receipt records that edit separately.

Build one `ContextPatch` with `contextPatch`. Each `replaceText target body`
uses an opaque target from `textTargets current`, binding the exact reference,
selector and owner-issued content version. `removeUnit` uses an issued target
from `unitTargets current`; eligible removals are whole nonopaque completed
exchanges or independently removable authored notes. Equal text does not identify
an occurrence. `appendNote body sources` creates an assistant note and preserves
its source references; the host issues its new reference at commit. `toNotes`
purely builds removals and assistant notes for selected removable exchanges.

`previewContext current patch` validates known fields, versions and overlapping
targets without effects, returning a projected document, changed targets and
before/after character counts. Its `previewInspectedAuthorship` retains the input
observation rather than issuing metadata for the proposed document; appended
notes have no reference until commit. It makes no provider token or cache estimate and
cannot establish current authority. Removing a group and editing a nested field
in the same patch is invalid; distinct multipart selectors remain independent.
`ContextWorkflow.inspectAndCurate` demonstrates this build, preview, stage flow.

`stageContext patch` returns `ContextStageRefused problems` or
`ContextStaged transaction`. One synchronous cell may stage one batch; build
all changes before staging. A staged handle is pending work, not a commit
receipt. Context reads continue to show the committed base. On whole-cell
success, the accepted context batch and explicit model/effort choices commit
together. A terminal transaction rejection rolls them back together. Failure or
cancellation discards the batch; external effects already issued remain. A stage
refusal accepts no transaction and leaves separately requested choices and any
already accepted batch intact. A patch never changes model or effort by itself;
choice-only cells remain supported. In a later cell,
`inspectContextTransaction transaction` reports staged, committed with the
actual head/generation and appended references, rejected, cancelled, or unknown.
A saved inspection or target preserves data and versions, never edit authority.
Staleness compares current content rather than an unrelated head generation;
returning to exactly the same content permits that version again.

Only an explicitly synchronous profile admits `ContextReadWrite`. After the
editing cell commits, capture the context for delegation separately;
`ContextWorkflow.delegateCuratedContext model actualSpec` uses that completed
projection. Its model value pins the captured context's family and selects the
child's exact model and effort. It requires `ForkAgent family`; the runtime checks
those choices before allocating a child.
`getInitialContext` reports historical startup evidence, so refresh selects its
exact reference in a new inspection and stages a targeted replacement.
`inspectOriginalText reference selector` observes an authorized retained original,
including after removal, with explicit missing, denied, unsupported-selector
and unavailable outcomes. It does not enumerate arbitrary transcript history.

`C.trimText reason retainedText` prefixes exact retained source with an ordinary
`[Trimmed: reason]` marker. Same-model continuation retains opaque reasoning.
Unsupported cross-model history is refused explicitly. `setNextModel` accepts a
typed `Model`; `setNextReasoning` accepts a provider-qualified `Reasoning` choice.
Both stage changes for the next request within the backend selected when the run
started. A custom model names that backend explicitly; neither operation switches
providers or establishes semantic validity after facts change.

For a compact semantic choice, `ContextWorkflow.selectOriginalSlices` uses
`J.each` to examine source slices together. It returns only selected original
text and its provenance; use the selected version-bound targets to build a
patch. Jev supplies a selection, never authority to open references.
