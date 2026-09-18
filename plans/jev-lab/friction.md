# Workbench friction log (shoal proxy session `lab`, 2026-09-17)

Every entry is something I hit while driving the proxy myself, with the exact
cell and the exact message.

## 1. A helper bound without a signature reports the error on a later line

    slurp p = Cmd.quiet (Cmd.run (Cmd.argv ["cat", p])) <&> (either (const "") id . Cmd.stdout)
    rawA <- slurp "..."
    pure (object [...])

The diagnostic pointed at line 3 (`pure`), not at `slurp` on line 1. It was in
fact line 3's fault (see 2), but a reader debugging a helper has no way to tell
the two apart.

## 2. `pure` on a cell's last statement is an ambiguity error

    Ambiguous type variable `f0' arising from a use of `pure'
    prevents the constraint `(Applicative f0)' from being solved.
    this declaration's type is ambiguous; give it a signature naming the type you meant

A cell's final unit is a value, not an action. Writing `pure (object [...])`
(the habit from a single `do` block, which does work) leaves `Applicative f0`
open. The appended advice says "give it a signature", which is the wrong repair:
the fix is to delete `pure`.

Fix: in `ambiguous_type_advice`, when the open constraint is `Applicative`/`Monad`
and the span is a use of `pure`/`return` as a top-level cell unit, say
"a cell's last statement is a value, not an action; drop `pure`".

## 3. The notebook preamble imports `(:&)` but not `Nil`

    <cell>:21:19-22: error:
        Data constructor not in scope: Nil :: Packet fs7 J.Questions

Every Jev packet ends in `Nil`. The workbench preamble
(`tidepool/src/actor_host.rs`) imported `Jev.Operators (Cell ((:=)), Packet ((:&)))`,
so a packet copied from a skill example or from `Project/Review.hs` fails on its
last line. Fixed: the import now names `Nil` too. Takes effect on the next
session launch.

## 4. A cell returning a `String` renders as an array of one-character strings

    ["'e'","'r'","' '","'0'", ...]

`show` on an answers record produced a `String`; the JSON renderer treated it as
a list. Returning `Text` or a `Value` is fine. Related to the known
"Text inside a Show'd record renders unquoted" item.

## 5. Declarations collide across cells; binds shadow. You cannot iterate on a definition.

    `symA` is ambiguous because `symA` was re-declared in this session
    (Tidepool.Session.Lib.G7 and Tidepool.Session.Lib.G8 both define it);
    reuse the earlier `symA` declaration instead of re-running it —
    never re-declare a type that already exists in this session

In a workbench the whole point is to refine a definition and re-run it. A
statement bind (`x <- ...`) shadows happily; a declaration (`f x = ...`) is a
hard error, and the advice tells the reader not to do the thing they are
deliberately doing. Worse, a cell that fails partway can leave its earlier
declarations committed, so the retry of the *same* cell collides with itself —
which is exactly what happened here after a command was refused by the mount
boundary.

Wanted: a later declaration shadows an earlier one, as in GHCi and as our own
repl server already documents for binds. Failing that, the advice should say how
to shadow rather than forbid the attempt.

## 6. (Positive) The mount boundary refusal is a good message

    this actor cannot run a command in /home/inanna/.claude/jobs/.../fix/a:
    working directory ... is outside the process mount boundary; it may run in
    /home/inanna/dev/shoal-evals/tui-test-app, and holds no worktree to write in

It named the boundary, the directory that would work, and the custody state. I
changed the cell correctly on the first read. Worth keeping as the model for
other refusals.

## 7. There is no way to turn Text into an Int

    Not in scope: `T.decimal'
    Note: The module `Tidepool.Data.Text' does not export `decimal'.
    Variable not in scope: reads :: String -> [(Int, b0)]

Parsing a line number out of `path:line:col` is routine in this work. Neither
`T.decimal` (the obvious name, from `Data.Text.Read`) nor `reads` nor `read` is
reachable from the default surface. I hand-rolled a digit fold:

    T.foldl (\a c -> a * 10 + (fromEnum c - 48)) 0 (T.filter (\c -> c >= '0' && c <= '9') l)

Wanted: `T.decimal`, or a `readInt :: Text -> Maybe Int` in the Prelude.

## 8. A declaration cannot see a value bound in the same cell

    excerptAt loc = ... (lookup (fileOf loc) blobs)
    <cell>:9:108-113: error: Variable not in scope: blobs :: [(Text, Text)]

`blobs <- ...` earlier in the same cell is invisible to a declaration in that
cell, because declarations compile into a separate plane. The fix is to write
the whole thing as one function that takes its inputs as arguments, which is
better style anyway — but nothing says so, and the natural first draft fails.

This interacts badly with item 5: the workaround is a bigger declaration, and a
bigger declaration is exactly what you cannot re-run after a typo.

## 9. Round-tripping typed answers through JSON loses to record dot

I first read the answers with `toJSON` and `KM.lookup`, and hit an
unresolvable numeric defaulting error comparing a JSON `Number` against `0.6`.
Reading them as `(J.answers r).each` and `p.must_change.yes` is shorter, typed,
and compiles. Worth saying plainly in the Jev skill: never `toJSON` an answers
packet except to log it.

## 10. An authored project module is invisible to the agent's own lookup

Given a task that mentioned `Project.Investigate`, the first thing the Sol root
did was ask the harness about it:

    lookup { "queries": ["doc workbench", "Cmd.shell", "Cmd.run", "Project.Investigate"] }
    lookup { "queries": ["doc command", "Cmd.command", ...] }
      doc command
        error: unknown Shoal documentation topic `command`; topics: tree
        (worktree), workbench, request, unfold, watch, deadline, refinement,
        lineage, cleanup, recovery, jev, actors

`lookup` resolves names from the shipped surface and `doc` lists a fixed set of
topics. Neither knows about the modules the workspace itself authors, even
though those modules are listed in `config.toml` and compiled into the session.
An agent can only find one if a human names the file path in the brief, which
defeats the point of shipping reusable project code for the next run to build on.

Wanted: `lookup` resolves names exported by workspace modules, and `doc` lists
each authored module alongside the built-in topics.

## 11. Shoal session launch is single-tenant per workspace, not per session name

    Error: Custom { kind: Other, error: "Shoal host failed: tidepool storage
    failure at .../actor-worktrees/2d9a56b8.../bindings/.owner.lock: another
    process already owns this binding root; stop its Shoal session and wait
    for shutdown before launching again (ownership was not changed); session
    \"lab7\" retained for inspection; native execution may still be running." }

Launching `lab7` against `--workspace $HOME/dev/shoal-evals/tui-test-app`
failed immediately: lab5 already held the binding root for that same
workspace path. lab6 and lab8 failed with the identical error at nearly the
same timestamp. The lock is keyed on the workspace's worktree-binding hash,
not on the `--session` name, so several distinctly-named sessions cannot run
concurrently against one shared `--workspace` directory; only one Shoal
session can be live against it at a time. A breadth survey that fans workers
out with distinct session names but a shared workspace path will serialize
at launch, not at the model layer, and workers 6/7/8 (at minimum) lost their
launch attempt entirely with no cell run. Waiting for lab5 to end and
retrying is the only path found; there is no `--workspace` isolation or
queueing.

## 12. A cell-level declaration silently collides with a stdlib name

    <cell>:68:9-14: error: Ambiguous occurrence `route'.
    It could refer to either `Tidepool.Actors.Shoal.route', imported from
    `Tidepool.Actors.Shoal' ... or `Tidepool.Session.Lib.G12.route',
    defined at <cell>:30:1.

`route` is an ordinary word and an obvious name for a function that routes
something. Nothing warns at declaration time; the collision surfaces at the
first *use*, three lines from the bottom of a long cell, and the message names
a generated module so it reads as an engine fault rather than a name clash.
Declaring a name that shadows an import should either be accepted with the
local winning, as GHCi does, or refused at the declaration with the suggestion
to rename.

## 13. The display budget is consumed by evidence, so a working cell fails at three items

A cell that routed one artifact rendered its result. The same cell over three
artifacts rendered `Array [String "4610b5` and stopped, having spent its
allowance on the states it had already sent to the model. The budget is
per-cell, not per-session: a one-line cell submitted immediately afterwards
displayed fine.

The failure mode is bad because the cell *succeeded* — it committed, the model
calls were made and paid for, and the answers are simply not visible. Nothing
says which binding consumed the allowance, and the obvious repairs (projecting
a smaller result, shortening the returned strings) do not help, because the
cost is in the evidence rather than the result. `Cmd.quiet` helps and is not
mentioned anywhere near the relevant advice.

Wanted: say what consumed the budget, and count a value sent to Jev separately
from a value displayed.

## 14. Truncating a check log from the front removes every diagnostic

Mine, not the engine's, but worth recording because it will happen to anyone.
`T.take 2500` of a `check.sh` log yields nix warnings and cargo progress; the
compiler diagnostics are at the end. A packet built that way asked a model
about preamble, and the model correctly answered that it contained no
diagnostics, which looked like a model failure and was not. Filter to
diagnostic lines, or take the tail.
