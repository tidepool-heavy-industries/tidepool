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

Build a reusable `textTurn` or `typedTurn`, then call `invokeModel turn input`.
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

`Tidepool.Agent.Context` provides an authority-free `Context` value for
reviewing and curating the current actor transcript. The runtime retains
authority: a reference in this value can identify a transcript item, but it
cannot open or restore one. `editableTexts` traverses authored text and eligible
visible message/result bodies, using full text rather than bounded previews.
Native Haskell tool input/source and function arguments stay pinned, even after
completion. Native grouping remains protected, while each visible body follows
its own editable flag. `visibleTexts`, `blockKind`, and `blockProvenance` let a
curator inspect content and origin. `toNotes` turns selected nonopaque completed
exchanges into authored notes with source provenance; pending and protected
groups remain untouched, and opaque group removal is refused.

Context reads and writes require `ContextReadWrite`, which only a synchronous
tool profile can declare. A normal asynchronous cell remains the default and
cannot edit context. The whole synchronous cell sees its staged edits; context,
model, and effort publish together only on whole-cell success. Failure or
cancellation discards the draft and deferred children, but external effects
already issued are not rolled back. Actor-owned background work may outlive a
successful cell.

`C.trimText reason retainedText` is a pure helper that prefixes the retained
source with `[Trimmed: reason]`. The marker is ordinary text, not runtime
metadata. Same-model continuation forwards opaque reasoning unchanged while
using edited visible text; this does not promise that earlier conclusions stay
semantically valid after facts change. An incompatible cross-model continuation
must fail explicitly instead of silently dropping opaque history. Stage a
model with `setNextModel "executor"` and effort with
`C.setNextEffort C.High` only when the context/model combination is supported.
The model name is `Text`: the host resolves a configured alias first, then
treats an unmatched value as a literal identifier.

The editable `Context` retains native evidence. Cross-model input can project
completed reasoning exchanges as attributed readable notes, including visible
summaries and tool inputs/results, when the provider context is compatible.
The Store keeps the originals. Incomplete or unauthenticated opaque exchanges
and incompatible compaction prevent switching; they are not converted into
empty summaries.

Use `display` on a returned `Context` for its bounded structural view of authored
blocks and safe native previews. `ContextWorkflow.inspectAndCurate` demonstrates
provenance-preserving notes and editing full eligible text. The current call,
pending operation identities/pairing, and later arrivals remain protected.
Each visible body has its own editability flag. A later cell can edit admitted
visible message/result bodies from a completed editing exchange; native tool
source/input and function arguments remain pinned.
Restoring a saved `Context` intentionally replaces the editable visible prefix,
but does not restore authority. It must preserve the current protected and
opaque groups; a stale snapshot missing required groups is refused. Persistent
Haskell helpers reduce later calls but do not make their source editable.

Keep curation in ordinary Haskell. For a compact semantic choice, `J.each`
can examine many source slices in one packet. `ContextWorkflow.selectOriginalSlices`
returns only exact selected text together with its original provenance; apply
those values through the ordinary optics. Jev supplies a selection, not
rewritten source text or authority to open transcript references.
