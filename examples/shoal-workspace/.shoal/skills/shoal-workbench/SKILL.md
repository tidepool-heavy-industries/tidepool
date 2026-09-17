---
name: shoal-workbench
description: Write Haskell notebook cells that typecheck the first time — Text vs String, Label vs Text, annotating polymorphic expressions, keeping observations small, multi-line operator chains, and shell arguments. Load when a cell was rejected or a display was truncated.
---

Send one cell of ordinary Haskell: declarations, bindings and expressions. GHC
splits declarations from statements and checks the whole cell before any effect
runs, so a rejected cell installs no bindings and runs nothing. Declarations are
mutually recursive and visible to every statement, but a declaration cannot
depend on a binding a statement in the same cell introduces. Every expression
displays its value; declarations and bindings persist into later cells.

```haskell
data Finding = Finding { findingPath :: Text, findingLine :: Int }
render :: Finding -> Text
render finding = findingPath finding <> ":" <> T.pack (show (findingLine finding))
let findings = [Finding "src/Retry.hs" 12, Finding "src/Fetch.hs" 44]
map render findings
```

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
note
```

A fully polymorphic expression — a bare `error "…"` or `undefined` — cannot be
classified as pure or effectful, and the cell is rejected before it runs.
Annotate it:

```haskell
let unreachable path = error ("no lane owns " <> T.unpack path) :: Text
("annotated, and never forced" :: Text)
```

`assignment` takes a validated `Label`, not free `Text`. A literal validates
when it is forced, so `assignment "revision" task` reads naturally, but a
`Text` computed at runtime needs `labelFromText`, which keeps a validation
failure as a value instead of throwing inside a launch. The same applies to
campaign, fork-group, request and watch labels.

```haskell
let laneLabel = "consumer-tests" :: Label
let dynamic = labelFromText ("lane-" <> T.pack (show (2 :: Int)))
(laneLabel, dynamic)
```

## Every bound value is observed

Each binding in a cell is displayed, and the cell shares one bounded display
allowance. Binding six whole files exhausts it before the interesting part of
the cell runs. Bind the short preview, not the file:

```haskell
previews <- forM ["README.md", "Justfile"] $ \path ->
  (path,) . T.take 2000 . either (const "") id . Cmd.stdout
    <$> Cmd.run (Cmd.withArguments [path] [bash|sed -n '1,40p' -- "$1"|])
map fst previews
```

The projection is the point: display the keys, keep the text in the binding for
the next statement. A truncated display offers `cellDisplay.more`, which reads
the next retained page without repeating the effect, and `Cmd.quiet action`
suppresses routine command presentation when only the data matters.

## Multi-line chains

A continuation line of a multi-line `let` must be indented past the bound name.
A line starting at the name's column begins a new binding and fails to parse.
This bites hardest on `:&` packet chains and on record updates:

```haskell
{-# LANGUAGE OverloadedLabels #-}
let packet =
      #enough := J.noul "Is the preview enough to judge the file?"
        :& #next := J.choice "Which file first?"
             (J.alt #none "No file in this set is on the path" ("" :: Text)
               J..| J.many [("retry", String "src/Retry.hs", "src/Retry.hs")])
        :& J.Nil
let described = "packet bound" :: Text
described
```

Parentheses around the whole chain work equally well and survive reindentation
better. The same rule governs `<$>`/`<*>` chains inside an `unfold`.

## Shell arguments are positional

`[bash|…|]` is literal Bash: Haskell interpolates nothing, and backticks and
`$VAR` mean what Bash means. Dynamic values go through `Cmd.withArguments`,
where the list positions become `$1`, `$2`, … in order — never string
concatenation into the script, which is how a path with a space or a quote
becomes a shell injection.

```haskell
let compare' old new = Cmd.withArguments [old, new] [bash|git diff --stat "$1" "$2"|]
Cmd.describe (compare' "HEAD~1" "HEAD")
```

`Cmd.describe` inspects the intent without executing. `Cmd.argv [program, a, b]`
bypasses Bash entirely when no shell features are wanted.

If a statement fails at runtime, its earlier bindings and completed effects stay
committed and the suffix is marked not run — read that receipt before deciding
whether the next cell is new intent. `doc workbench` is the same material in
fallback form.
