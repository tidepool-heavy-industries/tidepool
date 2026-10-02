Send one notebook cell of ordinary Haskell. A cell may contain
declarations, bindings, and expressions. GHC splits declarations from statements
and checks the entire cell before any effect runs. Declarations are mutually
recursive and visible to all statements; a declaration cannot depend on a binding
introduced by a statement in that same cell. Later statements can use earlier
bindings, and every expression displays its value.

```haskell
data Candidate = Candidate { candidateScore :: Int }
score candidate = candidateScore candidate
let candidates = [Candidate 7, Candidate 3]
let approved = filter ((>= 5) . score) candidates
map score approved
```

The cell summary counts declarations, statements, and expressions. Typecheck rejection
installs no bindings and runs no effects. A fully polymorphic expression (a bare
`error "..."` or `undefined`) cannot be classified as pure or effectful; annotate
it, e.g. `error "..." :: Text`. If a statement fails at runtime, its
earlier bindings and completed effects remain committed and the suffix is marked
not run. Inspect that receipt before deciding whether a new cell is new intent.

The `haskell` tool schedules cells asynchronously by default, and an
asynchronous-only host may expose no synchronous alternative. When a typed
agent spec declares `haskell_sync`, that cell waits for completion before its
caller continues to the next inference. The pause applies to that caller; it
does not wait for actor-owned deferred children. Only the synchronous typed
effect row includes `ContextReadWrite` and `setNextModel`.

Context and next-model edits staged by a synchronous cell commit together only
when the whole cell succeeds. A failed cell does not commit those edits, but it
cannot undo external effects already issued, such as a command or provider
request. For parent curation followed by delegation, finish the synchronous
parent invocation with `unfoldDeferred`; its actor-owned children then start
from the committed context. Do not await those children inside the invocation
that creates them. `bridge/haskell/examples/model-turns/ContextWorkflow.hs`
contains compiled examples of this workflow. `setNextModel` takes `Text`: the
host resolves a configured alias first, then treats an unmatched value as a
literal model identifier.

Repeated context reads in one invocation see its staged edits. Saving a
`Context` binding saves data, not edit authority: passing that value to
`putContext` in a later synchronous invocation intentionally replaces that
invocation's editable prefix. Its current call, completion and later arrivals
remain protected. After completion, the editing exchange becomes eligible for
curation by the next invocation. `editableTexts` skips native evidence; inspect
`blockKind` and use `toNotes` to convert selected completed exchanges before
editing their text.

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

Expressions share a bounded display allowance per cell. A truncated display
offers `cellDisplay.more`, which reads its next retained page without repeating the
original effect. Throughout a cell, `cellDisplay` refers to the previous cell's final
display; a cell with no display leaves it unchanged. A runtime failure retains
the last completed display in its prefix. Bind evidence you need to keep.

Ordinary `data` and `newtype` declarations get structural displays automatically.
Fields with a `Display` instance use it; unsupported fields are opaque, and
function fields show `<function>`. Explicit instances are preserved. GADT and
existential declarations require an authored instance when structural display
is needed.
Declarations and bindings persist between cells. Earlier closures retain the
definitions they captured; rebinding a name does not rewrite them.

skill: exomonad-workbench
