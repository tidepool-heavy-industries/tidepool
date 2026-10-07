Use notebook Haskell to build functions, small languages and the machines that
interpret them. Bind working data, introduce local records or sums, and connect
effectful functions with Kleisli composition (`>=>`) or `do`. The notebook's
`&&&`, `***` and `|||` compose effectful functions over products and sums;
they sequence effects, while `Tidepool.Async` overlaps independent waits.
`Control.Lens` is in scope for reusable focuses, updates and traversals.

Typed agent requests use the installed AgentSpec input and reply types. Record
actors interpret typed calls and events against persistent state; Jev alternatives can carry
values, closures or actions into their next transition. Keep one-off definitions
in the notebook and adapt them as you learn. See `exomonad-workbench` for the
composing vocabulary, `doc actors` for stateful machines and `doc jev` for semantic
glue.

Send one notebook cell of ordinary Haskell. A cell may contain
declarations, bindings, and expressions. GHC splits declarations from statements
and checks the entire cell before any effect runs. Declarations are mutually
recursive and visible to all statements; a declaration cannot depend on a binding
introduced by a statement in that same cell. Later statements can use earlier
bindings. Expressions and bindings retain typed values without rendering them;
use `display value` when you want bounded structured output.

```haskell
data Candidate = Candidate { candidateScore :: Int }
score candidate = candidateScore candidate
let candidates = [Candidate 7, Candidate 3]
let approved = filter ((>= 5) . score) candidates
display (map score approved)
```

The cell summary counts declarations, statements, and expressions. Typecheck rejection
installs no bindings and runs no effects. A fully polymorphic expression (a bare
`error "..."` or `undefined`) cannot be classified as pure or effectful; annotate
it, e.g. `error "..." :: Text`. Successful completion publishes the cell's
executed declarations and bindings together. Runtime failure or cancellation
before publication publishes none of its names; earlier successful cells remain
available. Completed effects retain their receipts, and explicitly transferred
captures retain their independent ownership. The unexecuted suffix is marked not
run. Publication that already committed survives later cleanup trouble; inspect
the publication and effect receipts before deciding whether to submit new intent.

The `haskell` tool schedules cells asynchronously by default, and an
asynchronous-only host may expose no synchronous alternative. When a typed
agent spec declares a synchronous notebook such as `haskell_sync`, that cell
waits for completion before its caller continues to the next inference. The
default notebooks share the same effects; the spec can declare separate effect
profiles. The pause applies to that caller; independently owned work follows its own
lifecycle. Only an explicit synchronous profile can include
`ContextReadWrite`, which provides context
editing, `setNextModel`, and `setNextEffort`.

Context, next-model, and next-effort edits staged by a synchronous cell commit
together only when the whole cell succeeds. A failed cell does not commit those edits, but it
cannot undo external effects already issued, such as a command or provider
request. Captured contexts are snapshots. To use a completed edit as another actor’s
starting context, explicitly capture and pass that context. `setNextModel` takes `Text`: the
host resolves a configured alias first, then treats an unmatched value as a
literal model identifier. `C.setNextEffort C.High` changes the next request’s
reasoning effort while preserving the model and existing context prefix.

The editable context retains native evidence. For another model’s inference,
completed reasoning exchanges can become attributed readable notes with their
visible summaries, calls, and results when the provider context is compatible;
the Store retains the originals. Incomplete or unauthenticated opaque exchanges
and incompatible compaction prevent a model switch rather than silently discard
evidence.

Repeated context reads in one invocation see its staged edits. Saving a
`Context` binding saves data, not edit authority: passing that value to
`putContext` in a later synchronous invocation can intentionally replace that
invocation's editable visible prefix. The candidate must preserve current
protected and opaque groups; a stale snapshot missing required groups is
refused. This restores data, not authority. The current call, pending operation
identities/pairing and later arrivals remain protected. Each visible body has
its own editability flag, so an eligible result body can be edited without
changing its group's protected structure. After completion, an editing
exchange's admitted visible message/result bodies become eligible in a later
cell. Native Haskell tool input/source and function arguments remain pinned;
grouping stays protected while each visible body follows its own editable flag. Retained
helpers make later calls shorter but do not make their source editable.
`editableTexts` uses full visible text, never bounded previews. Use
`C.trimText reason retainedText` to keep exact source beside an ordinary
`[Trimmed: reason]` marker. `toNotes` preserves provenance for selected
nonopaque completed exchanges; opaque group removal is refused.

Same-model continuation forwards opaque reasoning unchanged, although edited
facts may make earlier conclusions stale. Unsupported cross-model opaque history
fails explicitly. Explicitly owned background work may outlive successful
settlement. Other agents receive only the context and dependencies selected for
them. External effects already issued by a failed cell are not rolled back.

Launching actor-owned background work does not prevent a normally completed
cell from committing. The editing computation itself must finish successfully;
an invocation that transfers its reply, backgrounds its own computation or is
cancelled does not commit unfinished edits.

Imports persist for later cells. Leading `LANGUAGE` and `OPTIONS_GHC` pragmas are
normalized by GHC and apply only to this cell. Put neither pragmas nor imports
after executable source. CPP and custom preprocessors are unavailable. Cells do
not accept colon commands or `:{` / `:}` delimiters.

Opaque functions are useful values. Ask `lookup` for a name or use a
`::type` query to search callable names (wildcard unknown parts with `_`, e.g.
`:: Cmd.Command -> _`). Query `doc` for the available guides or
`doc workbench` for this one. The `status` tool defaults to `summary`; its
`detailed`, `recovery`, `lineage`, `trace`, and `bindings` views answer runtime
questions without disturbing the cell.

The hosted `lookup` tool is not a Haskell function. From a cell, use
`lookupRaw (lookupRequest ["Cmd.quiet"])`; `lookupRequest` supplies the shipped
hosted tool's defaults. Use `LookupRequest` directly for custom lookup options.

`display value` emits bounded structured output and returns an opaque
`DisplayHandle value`. `expansions handle` lists the available field keys and
their labels; `expand handle key` emits that field's bounded detail and returns
an updated handle. Keys belong to one display and are opaque values. Use
`display (show value)` when you want Haskell's textual `Show` form. Ordinary
`data` and `newtype` declarations get structural displays automatically. Fields
with a `Display` instance use it; unsupported fields stay opaque, and function
fields show `<function>`. Explicit instances are preserved. GADT and existential
declarations require an authored instance when structural display is needed.
Declarations and bindings persist between cells. Earlier closures retain the
definitions they captured; rebinding a name does not rewrite them.

skill: exomonad-workbench
