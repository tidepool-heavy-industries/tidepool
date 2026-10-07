---
name: exomonad-workbench
description: Write Haskell notebook cells that typecheck the first time — Text vs String, Label vs Text, annotating polymorphic expressions, explicit value display, multi-line operator chains, and shell arguments. Load when a cell was rejected or a display needs expansion.
---

Send one cell of ordinary Haskell: declarations, bindings and expressions. GHC
splits declarations from statements and checks the whole cell before any effect
runs: typecheck rejection installs no bindings and executes nothing. Successful
cells publish declarations and bindings together. Runtime failure or cancellation
before publication publishes no cell names; completed effects and independently
owned captures retain their receipts. Inspect the outcome before retry. Declarations are
mutually recursive and visible to every statement, but a declaration cannot
depend on a binding a statement in the same cell introduces. Expressions and
bindings retain typed values without automatic rendering; declarations and
bindings persist into later cells. Use `display value` for bounded structured
output, and `display (show value)` for Haskell's textual `Show` form.

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

## Text, not String

`Text` is the currency of this workbench: paths, labels, output, messages. The
one place `String` appears is `show`, so the conversion you will write over and
over is `T.pack . show`. Going the other way is `T.unpack`, and it is almost
always a sign that something should have stayed `Text`.

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

Agent labels are optional ordinary `Text` used for description. They do not
select an actor, workspace, permission, or group. Agent input and reply types
come from the actual installed `AgentSpec`; use raw typed values for requests.

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
better. The same rule governs `<$>`/`<*>` chains over typed `Await` values.

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

## Effectful reads bind with `<-`, never `let`

`readFile :: FilePath -> Eff effs (Either FsError Text)` — `FilePath` is `Text`
here, and the result is an action, so `let d = readFile p` binds the action,
not the text, and every later use is a type error a `T.pack` cannot rescue.
Bind it, and pattern-match the success in the bind:

```haskell
Right src <- readFile ".exomonad/config.toml"
display (T.take 200 src)
```

A refutable bind like this fails the statement when the read fails, which is
usually what you want in a cell; use `either` when the failure is a value you
carry forward. Nothing here is ever `String`: `T.lines`, `T.splitOn`,
`T.stripPrefix` do the path and output work.

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
