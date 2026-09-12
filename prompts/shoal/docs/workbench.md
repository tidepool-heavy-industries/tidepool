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
installs no bindings and runs no effects. If a statement fails at runtime, its
earlier bindings and completed effects remain committed and the suffix is marked
not run. Inspect that receipt before deciding whether a new cell is new intent.

Imports persist for later cells. Leading `LANGUAGE` and `OPTIONS_GHC` pragmas are
normalized by GHC and apply only to this cell. Put neither pragmas nor imports
after executable source. CPP and custom preprocessors are unavailable. Cells do
not accept colon commands or `:{` / `:}` delimiters.

Opaque functions are useful values. Ask hosted `lookup` for a name or use a
`::type` query to search callable names. Query `doc` for the available guides or
`doc workbench` for this one. The `status` tool defaults to `summary`; its
`detailed`, `recovery`, `lineage`, `trace`, and `bindings` views answer runtime
questions without disturbing the cell.

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
