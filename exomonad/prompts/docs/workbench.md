Use notebook Haskell to build functions, small languages and the machines that
interpret them. Bind working data, introduce local records or sums, and connect
effectful functions with Kleisli composition (`>=>`) or `do`. The notebook's
`&&&`, `***` and `|||` compose effectful functions over products and sums;
they sequence effects, while `Tidepool.Async` overlaps independent waits.
`Control.Lens` is in scope for reusable focuses, updates and traversals.

Typed agent request sites select their own input and reply types; the installed
AgentSpec supplies tools and effects. Record actors interpret typed calls and
events against persistent state; Jev alternatives can carry
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

An accepted context batch and explicit next-model/effort choices in a synchronous
cell commit together only when the whole cell succeeds. A terminal transaction
rejection rolls them back together. A patch never changes model or effort by
itself, and choice-only cells remain supported. A stage refusal accepts no
transaction: it leaves separately requested choices and any first accepted batch
intact. A failed cell does not commit its staged edits, but it
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

`C.getContext` reads the active committed projection, including full visible
text and owner-issued content versions. Repeated reads in one cell keep that
committed base. `contextBlocks` is read-only; `visibleTexts` and `referencedTexts`
inspect bodies, exact references and selectors. `contextAuthorship` is a read-only
getter of owner-observed role and authored-note status for each exact field;
it contains no copied text and does not infer roles from content. `editableTexts` is a read-only
fold over eligible bodies. Tool source/input and function arguments stay pinned.

Build one batch with `C.contextPatch`: use `C.replaceText target body` on targets
from `C.textTargets current`, `C.removeUnit unit` on `C.unitTargets current`, and
`C.appendNote body sources` for assistant notes. Targets retain exact references,
selectors and content versions; they grant no authority. Removal targets cover
whole eligible completed exchanges and independently removable notes. Opaque
and protected groups remain protected. `C.toNotes` builds a pure patch for
selected removable exchanges. A group removal and a nested field edit overlap
and must not appear in the same batch.

`C.previewContext current patch` returns typed problems or a rich projected
document with changed targets and before/after character counts. It checks known
fields and versions without effects. Preview authorship retains the original
inspection observation, not newly issued metadata for the proposed document;
appended notes have no reference yet. It does not predict provider tokens or
cache reuse. `display` shows bounded text previews, references and versions.
`C.stageContext patch` returns `C.ContextStageRefused problems` or
`C.ContextStaged transaction`. One cell stages one batch. A staged identifier is
pending work; inspect it in a later cell with `C.inspectContextTransaction` to
observe `C.ContextTransactionCommitted receipt`, rejection, cancellation or
unavailability. The receipt names the actual head, generation, change flag and
new note references. A refused second stage leaves the first batch intact.
Saved inspections preserve versions and data, never future edit authority.
Current content versions detect changed targets while accepting an unchanged
target across unrelated commits, including a return to identical content.

Use `C.trimText reason retainedText` to keep exact source with an ordinary
`[Trimmed: reason]` marker. `C.inspectOriginalText reference selector` observes an
authorized retained original, even after its removal, distinguishing available,
missing, denied, unsupported and unavailable results; it does not browse the
whole original history. `getInitialContext` reports historical startup evidence.
Refreshing it requires a new inspection and replacement of that exact issued
reference; unrelated notes remain independent. Capture context for another actor
after the editing cell commits.

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
