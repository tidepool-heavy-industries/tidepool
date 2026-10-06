---
name: exomonad-workbench
description: "Compose notebook programs with Kleisli arrows, optics, local data types, closures and effectful continuations. Use to invent a small task language and its interpreter, connect agent RPC and Jev, inspect retained values, or resolve a cell's types and layout."
---

Work directly in ordinary Haskell. Define a local vocabulary for the problem,
compose a few functions, run them, inspect a useful projection, and keep building
on the retained values. Author small languages and the machines that interpret
them: commands and events in data types, behavior in functions, evolving state
in a record actor. A notebook definition can be useful for a single task; it
needs no library, tool registration or general-purpose framework. Tuples,
records, sums, closures and partially applied functions are all working material.
Introduce a type when it gives the next composition a useful shape or name.

The APIs in this skill are **shipped** and need no project module. Most names
are already in scope; the raw lookup API below and `Jev` and `Commands`
(effect types from `Tidepool.Effects.Core`) need imports. Names from
`.exomonad/workspace/Project`, such as
`coordinationActor`, `Outcome` and `Candidate`, are **example-only** and are
not used here.

```haskell
data Finding = Finding { findingPath :: Text, findingLine :: Int }
renderFindingLocation :: Finding -> Text
renderFindingLocation finding = findingPath finding <> ":" <> T.pack (show (findingLine finding))
let findings = [Finding "src/Retry.hs" 12, Finding "src/Fetch.hs" 44]
display (map renderFindingLocation findings)
```

## Compose behavior and data

An effectful function `a -> Eff effects b` is a Kleisli arrow. Chain stages with
`>=>` or `<=<`; use `do` when naming intermediate results makes the composition
clearer. The notebook's `&&&` feeds one input to two effectful functions and pairs
their results; `***` transforms the two sides of a pair; `|||` routes an `Either`
to the matching function. `firstK` and `secondK` transform one side while carrying
the other along. These operators act directly on effectful functions and sequence
their effects. Choose `Tidepool.Async` when independent waits should overlap.

Products combine information; sums choose a continuation. Use a record to carry
an observation and its source together, or a local algebraic data type to name
the outcomes your next function handles. Agent RPC (remote procedure calls)
uses the same idea: choose an assignment and reply type, let a child compute the
reply, then map, match or fold it like any other value. The request supplies
`sessionInput` and `respond`; `exomonad-unfold` covers composing child branches.
Request inputs and replies use Haskell types without JSON/schema derivations.
Hosted `AgentSpec` tools and `typedTurn` have their own schema boundary; a child
branch also needs a displayable assignment for its activation preview.

Optics make a focus reusable. `Control.Lens` is in scope: lenses focus fields,
prisms select constructors, traversals reach several nested values, and folds
collect observations. Compose a focus once, then use `view`/`preview`, `over`,
`toListOf` or `traverseOf` to read, transform, collect or act at that focus.
`_1`, `_2`, `_Just`, `_Right`, `traversed` and `filtered` combine with JSON
`key`, `values` and `_String`; `lens`, `prism'` and `iso` build custom focuses.
Keep paths or indices alongside findings when the next stage needs to locate
them. Record access and pattern matching are convenient for a one-off projection;
optics are convenient when the focus itself composes or travels as an argument.

Ordinary lists support `map`, `filter`, folds and comprehensions. `traverse` or
`forM` sequences an effectful operation over the elements; `foldM` threads an
evolving state, and `wither` combines an effectful transformation with selection.
Applicative products combine independently specified inputs; monadic bind lets
one result determine the next computation. Use `unfold` for agent branches and
`Tidepool.Async` for invocation-local concurrency.

Functions, closures and `Eff effects result` actions are values too. Build a
reusable action or a list of candidate continuations before deciding which to
run; bind with `<-` when the result is needed. A retained result is reusable
data, while sequencing a retained action again repeats its effects. Prefer
`Member Effect effects` constraints for reusable helpers so they compose wherever
their effects are available; pin the concrete effects at an endpoint or actor
definition.

Use signatures and typed holes to expose the missing relationship when inference
is ambiguous. Lazy values and higher-order functions need not be rendered in full:
keep them bound and display the projection relevant to the next decision. For
Jev's heterogeneous packets and typed action selection, load `exomonad-jev`.
Let a Jev alternative carry a value, a closure or an action: semantic selection
can connect directly to the next stage. A record actor can retain the same
functions and data and interpret commands as events arrive; see
`exomonad-define-actors`.

## Cell execution and display

Send declarations, bindings and expressions as raw Haskell. Declarations are
mutually recursive and visible to every statement, but cannot depend on a
same-cell statement binding. Imports, declarations and bound values persist
after a successful cell. Values stay bound without automatic rendering; use
`display value` for bounded structured output or `display (show value)` for
Haskell's textual `Show` form.

GHC checks the whole cell before execution. Rejection runs nothing; runtime
failure or cancellation before publication installs no cell names. Completed
effects and independently owned captures remain real, so inspect their receipts
before retrying. Successful cells publish their declarations and bindings together.

## Look up a name from a cell

The hosted `lookup` tool is not a Haskell function. Import the raw lookup API,
then use its default request constructor:

```haskell
import Tidepool.Lookup (lookupRaw, lookupRequest)
found <- lookupRaw (lookupRequest ["Cmd.quiet"])
display found
```

`lookupRequest` uses the shipped hosted tool's defaults; use `LookupRequest`
directly when a request needs custom discovery, view, candidate limit, or references.

Leading `LANGUAGE` and `OPTIONS_GHC` pragmas apply to this cell only and must
come first; imports persist. No pragmas or imports after executable source, no
colon commands, no `:{` / `:}`.

## Text at the API boundary

Workbench paths, labels, output and messages use `Text`. `show` returns `String`;
use `T.pack . show` when its result feeds a `Text` API. `T.unpack` converts in
the other direction when composing with a function that expects `String`.

```haskell
let attempts = 3 :: Int
let note = "retry budget " <> T.pack (show attempts) <> " exhausted" :: Text
display note
```

A fully polymorphic expression — a bare `error "…"` or `undefined` — cannot be
classified as pure or effectful, and the cell is rejected before it runs.
Annotate it:

```haskell
let unreachable path = error ("no owner for " <> path) :: Text
display ("annotated, and never forced" :: Text)
```

`assignment` takes a validated `Label`, not free `Text`. Static assignment
labels use `[label|revision|]`, which is checked at compile time. A `Text`
computed at runtime needs `labelFromText`; handle its `Either` before launch.
Campaign, fork-group and watch labels have their own constructors and validators.

```haskell
let laneLabel = ([label|consumer-tests|] :: Label)
let dynamic = labelFromText ("work-" <> T.pack (show (2 :: Int)))
display (laneLabel, dynamic)
```

## A cell splits into units

A cell splits at column-1 lines into units that run in order. When a later unit
fails, the receipt names what the earlier units did ("unit 1 submitted the
reply", "unit 2 bound x"); read it before resubmitting. Keep `respond value` on
one line with nothing after it: a trailing `.`, `$` or backquoted operator is
rejected as a dangling operator, and a stray `) :: Text` on the next line is a
separate unit that fails to parse.

## Keep bindings and display selected evidence

Bindings retain typed values without rendering them. Keep large command results
bound, project the fields you need, and explicitly display a small preview:

```haskell
previews <- forM ["README.md", "Justfile"] $ \path ->
  (path,) . fmap (T.take 2000) . Cmd.stdout
    <$> Cmd.run (Cmd.withArguments [path] [bash|sed -n '1,40p' -- "$1"|])
previewHandle <- display (map fst previews)
display (expansions previewHandle)
```

The preview retains read failures as `Left`; fetch complete text before judgments
that require it. `display` returns a handle; `expansions handle` lists available
field keys and labels, and `expand handle key` inspects one field. The key belongs
to that display handle. `Cmd.quiet action` suppresses command event output when
only data matters.

## Multi-line chains

The notebook supports ordinary multi-line `let` layout, including multiple
value bindings in one group. A helper's signature and equation can also share
one explicit `let f :: T; f = ...` statement. Whole-cell rejection still runs
nothing.

A continuation line of a multi-line `let` must be indented past the bound name.
An operator continuation at the name's column is invalid layout.
This bites hardest on `:&` packet chains and on record updates:

```haskell
let packet =
      #enough := J.noul "Is the preview enough to judge the file?"
        :& #next := J.choice "Which file first?"
             (J.alt #none "No file in this set is on the path" ("" :: Text)
               J..| J.many #file fst snd [("src/Retry.hs", "the retry loop and its backoff")])
answer <- J.ask (J.rawState (String "one file, one preview")) packet
display (either (const ("packet bound" :: Text)) (const "answered") answer)
```

Parentheses around the whole chain work equally well and survive reindentation
better. The same rule governs `<$>`/`<*>` chains inside an `unfold`.

A packet stays polymorphic in whether it holds questions, answers or state
fields, so one bound and never asked has no mode to settle on and fails to
compile. Ask it in the same cell, as above, or pin the binding with a signature.

## Shell arguments are positional

`[bash|…|]` is literal Bash: Haskell interpolates nothing, and backticks and
`$VAR` mean what Bash means. Dynamic values go through `Cmd.withArguments`,
where the list positions become `$1`, `$2`, … in order — never string
concatenation into the script, which is how a path with a space or a quote
becomes a shell injection.

```haskell
let compare' old new = Cmd.withArguments [old, new] [bash|git diff --stat "$1" "$2"|]
display (Cmd.describe (compare' "HEAD~1" "HEAD"))
```

`Cmd.describe` inspects the intent without executing. `Cmd.argv [program, a, b]`
bypasses Bash entirely when no shell features are wanted.

## A signature and its equation travel together

A top-level declaration takes its signature on the line directly above the
equation, in the **same cell item**. A signature alone in an item is compiled
as a module with no binding: GHC says it "lacks an accompanying binding", the
name is never installed, and the next item reports `Variable not in scope`.

Inside a `let`, put both in one item — `let f :: T -> U; f x = …`, or the
signature and the equation aligned at the same column of one `let` block.
Splitting the signature onto its own `let` line loses the argument scope and
produces a `Variable not in scope` for the argument, not for `f`. This is the
fix for "this declaration's type is ambiguous; add a signature": the signature
has to land on the binding, not beside it.

```haskell
severity :: Int -> Text
severity n = if n > 2 then "high" else "low"
let inline :: Int -> Text; inline n = "work " <> T.pack (show n)
display (map severity [1, 3 :: Int] <> map inline [7 :: Int])
```

Pin a polymorphic result the same way at the use site. `knownEffects` in an
`R.definition` needs `:: ActorSpec MyActor MyEffects` on the definition, and a
handler helper bound outside its record needs
`:: … -> Handler MyState MyEffects ()`.

## A bare literal under `ToJSON` needs its type

`object [...]` accepts anything encodable, so a bare string literal has no type
to settle on and the cell is rejected as ambiguous before it runs. Annotate the
literal, not the call:

```haskell
let state = object ["owned_path" .= ("src/app.rs" :: Text), "changed" .= (2 :: Int)]
display state
```

The diagnostic for this reads the same as the one for an ambiguous function,
but the fix is different: here nothing needs a signature, one literal needs
`:: Text`.

## Identity types come from their constructors

A branch or a ref is a typed identity, not free `Text`. Construct it, and take
the `Text` back out by matching:

```haskell
let onto = mkBranchName "integration/tags"
let from = GitRef "exomonad/integration"
display (case onto of BranchName b -> b, case from of GitRef ref -> ref)
```

`atRef (GitRef "exomonad/integration")` is the deliberate committed seed for a
fork; `projectHead` and `currentCheckout` are the live ones. `currentCheckout`
resolves for the executing actor. If your build carries
`IsString` for these types, a bare literal works too — the constructor form
works either way.

## Bind a result with `<-`; retain an action with `let`

`readFile :: FilePath -> Eff effs (Either FsError Text)` — `FilePath` is `Text`
here. `let readIt = readFile p` retains an action for later composition; it does
not read the file. If the next step needs the text, sequence the action with
`<-` and handle its `Either`. `T.pack` cannot turn an unexecuted action into
its result. The following refutable bind stops this cell on a failed read:

```haskell
Right src <- readFile ".exomonad/config.toml"
display (T.take 200 src)
```

A refutable bind is useful when failure must stop the dependent suffix. Use
`case` or `either` when failure should select recovery or remain in the returned
evidence. `T.lines`, `T.splitOn` and `T.stripPrefix` compose directly with the
`Text` returned by these APIs.

`Cmd.stdout` returns `Either OutputIssue Text`. Retain that result and handle
the failure explicitly; substituting empty text would hide missing evidence.
The example renders a short failure notice while keeping the issue in `out`:

```haskell
out <- Cmd.stdout <$> Cmd.run (Cmd.withArguments ["HEAD"] [bash|git show --stat --oneline "$1"|])
display (either (const "not visible from here") (T.take 2000) out)
```

Bind first and project after. Explicit display emits only the selected bounded
value; use `Cmd.output` and `Cmd.next` to navigate retained command output without
rerunning the command. Extracting one field per statement out of a long value
costs a statement each time; bind the value once and project in one expression.

If a statement fails at runtime, none of that cell's names become public and the
suffix is marked not run. Completed effects remain real: recover command output
through its retained session ID, without rerunning the command. A successfully
published cell remains published even if later cleanup is uncertain. Read the
receipt before submitting new intent. `doc workbench` is the same material in
fallback form.
