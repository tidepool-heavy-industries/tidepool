# Change proposals from the Astra flight of 2026-09-17/18

Written for Astra to read and answer. Mark each: agree, disagree, or wrong shape,
and say what is missing. The execution plan is `astra-fix-waves.md`.

## Repairs

1. **A result too large to display still binds.** A bind keeps its retained value
   when materializing it for display exceeds the budget. The audit cell and
   `reflect 3` both bind. A regression covers an effectful result and Reflect
   history, and asserts the command ran once.
2. **An oversized bare expression is bound and shown bounded.** It gets a name, a
   bounded rendering, and the existing paging for the rest.
3. **Receipts distinguish three states.** The effect failed; or the effect
   completed and observing its result failed; or the status is genuinely unknown.
   The runtime does know which. `reflect` was labelled Unknown because a later
   error was attributed to an effect whose response had already been delivered.
4. **Qualified type and constructor lookup.** Your patch is adopted and finished:
   the tool description still described the old behaviour; a module browse cost
   two round trips (both queries go in one batch); a failed fallback round threw
   away the first round's results; tests are added for a missing qualified name
   and for a name that is both type and constructor. Your command-skill patch is
   adopted too, with the `Cmd.readOutput`/`Cmd.next` route for failed commands.

## Tiny mysteries

5. **Source order works within a cell.** A declaration after a statement sees
   that statement's bindings. Step one rejects with a targeted message and also
   catches the silent case where the name resolves to something else in scope.
   Step two, actual source-order execution, is an experiment.
6. **A few recognized errors name the next valid operation.** `Cmd.stdout` applied
   to a completion event; `String` where the stdlib wants `Text`. GHC is not
   interpreted in general.
7. **Lookup returns a starting point.** Signature, availability label, and one
   link to a worked usage topic, derived from the shipped examples. A `no match`
   suggests the nearest exported names under that qualifier.
7a. **`replace` already exists.** You wanted a handler revisable while work
   continues and started a second collector instead.
   `haskell/lib/Tidepool/Actor/Record.hs:361` exports
   `replace :: ActorHandle api -> ActorSpec api effects -> Eff parent (ActorHandle api)`.
   It is a discovery failure, not a missing mechanism: two skill files mention it
   and no worked example uses it. We add the example and the lookup pointer.

## Primitives the harness will own

8. `Cmd.readStderr`, and one call returning both complete streams with outcome and
   cleanup, valid for failed commands.
9. A guarded text replace that compares prior content before writing.
10. A `Text`-returning show.

## Crossing from direct tools into programs

11. Every direct shell call leaves a retained job in Haskell scope, named in the
    tool result.
12. Notices carry attempt identity and the retained handle. A job is marked
    superseded only where existing ownership data knows that.
13. A what-is-live view in the status tool: which collector watches which job and
    whether it finished; bindings with their defining execution; the binary's
    revision against the worktree; frozen modules that differ from disk. A view
    over existing data, not a registry.
14. Binding source as a value: exact text, revision identity, unresolved
    dependencies. No packaging, no edit-and-rerun tool.
15. Compact default rendering with obvious expansion.

## Underneath

16. Structured tracing: spans from run to actor, tool call, cell, unit and effect,
    with durations; cell source, receipts, lookup queries and GHC diagnostics in a
    run-local trace. First use: explain the slow first cells and the one
    26-second compile.
17. Read-only children as cheap as native subagents. You delegated six times to
    native subagents and once to a Shoal child. We measure why, then offer one
    call that takes a task and returns a reply.
18. The new jev-dsl, pinned as workspace source. The Jev doc and skill are
    rewritten from its authoring guide and the Guard example; every example is a
    tested cell. Your two cautions go in: a receipt is not a reason for strict;
    rules belong in Haskell and judgments in Jev.

## Not doing

19. Your own helpers stay yours: the receipt type, gather-before-drafting, the
    stopping policy, a run-with-followup helper.
20. No edit-a-draft-cell tool. Loading Haskell you write into a live session is
    deferred.

## Questions back

- Which three of these would change your next flight the most?
- Is item 11 right? Would a job binding you never asked for help, or clutter scope?
- For item 17, what made the native subagent the easier choice at that moment?
- You proposed a revisable decision tree and said Shoal's replacement mechanism
  would have to be used correctly. `replace` is at
  `/home/inanna/dev/tidepool-jev/haskell/lib/Tidepool/Actor/Record.hs:361`. Did
  you see it? If so, what stopped you using it?
- When you replace a running collector's handler, what should happen to work
  already in flight? Name the behaviour you want, not the one you would tolerate.
- Where should a shared question battery live so two consumers import the same
  one: a module under `/home/inanna/dev/tidepool-astra/.shoal/Project/`, a file
  under `/home/inanna/dev/tidepool-astra/.shoal/discoveries/`, or elsewhere?
- Your port candidates are saved as
  `/home/inanna/dev/tidepool-astra/.shoal/discoveries/select-useful-example.hs`
  and `/home/inanna/dev/tidepool-astra/.shoal/discoveries/run-ahead.hs`. Are those
  the right starting points, or is a live version better?
- Which line of run-ahead's paging would you delete first, given one call that
  returns both complete streams with outcome and cleanup?
- `/home/inanna/dev/tidepool-astra/.shoal/checks/reflex-unresolved.hs` routes
  unresolved names to inspection, but three other files still describe the removed
  add-import step. Do you want to finish that, or should we?
- For the small revisable do-the-obvious-thing program: what is the smallest
  version that would survive one real failure and one real repair?
