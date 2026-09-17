# Potential Jev notebook examples: Bash and code exploration

Recorded 2026-09-16. These five examples extend the
[notebook microprogram mockups](jev-notebook-microprograms.md). They are potential
authoring experiences, not compiled Haskell, implemented LSP APIs, or tested
workflows. Assume Jev and a suite of code-exploration tools are available.

Follow-on [paired live simulations](../jev-integration/BASH-LSP-EXPERIMENTS.md)
exercise every workflow with a positive and missing-candidate case. They run Jev
against synthetic tool observations; the APIs below remain prospective.

The aim is to compress repeated tool → frontier-model interpretation → tool
round trips into one model-authored notebook cell. Bash performs repository and
process operations; LSP supplies precise code relationships; Jev selects meaningful
next steps; Haskell retains values, evidence, budgets, and control flow. Internal
commands and inference calls still happen and take time.

`pick` and `relevant` below are proposed application helpers over the single `jev`
operation, not additional effect operations. `pick` selects an actual typed
candidate, with no-match and ambiguity outcomes. `relevant` judges candidates
individually and applies an authored policy. Descriptions go to Jev; local payloads
retain symbol handles, source ranges, test IDs and command values. Jev does not
generate commands, paths or relationships.

The sketches elide typed failure handling for readability. All traversals and
command execution need explicit budgets; missing candidates, incomplete output,
changed source or unavailable tool support return inspectable gaps. LSP methods
are illustrative adapter operations, not promises that every language server
exposes the same information. Compiler type locations may require tool-specific
resolution. Commands use argv or safely bound positional arguments.

## 1. Trace a symptom across abstraction layers

Inquiry: “Why can this request finish without notifying its caller?”

```haskell
logs  <- bashJson failingReproduction
event <- pick symptom (parseEvents logs)
start <- lsp.definition event.sourceLocation

trail <- walkBounded 5 start $ \symbol -> do
  body  <- source.read symbol.range
  calls <- lsp.outgoingCalls symbol
  pick
    "Follow the call that determines whether this result reaches its caller"
    (describeCalls body calls)

tests <- lsp.references trail.lastSymbol >>= enclosingTests
test  <- pick "Exercise the path just traced" tests
run   <- bash test.command

pure (trail, run)
```

The cell returns a source-backed path and a test observation. Small semantic
choices navigate wrappers, dispatchers and adapters; deterministic code tracks
visited symbols, handles terminal nodes and limits depth. Reference-to-test
mapping is an adapter/helper responsibility and may return no suitable test.

Traditional interaction compressed: inspect reproduction output, locate the entry
point, follow several calls, locate a relevant test, and execute it.

## 2. Turn a changed contract into a focused verification run

Inquiry: “Check the consumers that matter for this signature change.”

```haskell
diff     <- bashJson changedSource
changed  <- lsp.symbolsIntersecting diff.ranges
contract <- pick task changed

refs      <- lsp.references contract
consumers <- relevant
  "This caller depends on the changed behavior, not merely the unchanged name"
  =<< source.describeReferences refs

tests <- discoverTests consumers
chosen <- relevant
  "Exercises an affected behavior with useful assertions"
  tests

results  <- runWithin testBudget (map testCommand chosen)
failures <- traverse inspectFailure (failed results)

pure (Verification consumers results failures)
```

`inspectFailure` can itself select a diagnostic, resolve its symbol through LSP,
and fetch relevant source. Retain excluded candidates and coverage limits: a
focused selection is not exhaustive verification. Test execution remains under
the existing command/resource owner and a concrete execution budget.

Traditional interaction compressed: inspect diff, enumerate callers, inspect
consumer behavior, choose tests, execute checks, and localize failures.

## 3. Find a reusable implementation and check whether it fits

Inquiry: “Before adding this helper, find the closest existing mechanism.”

```haskell
hits      <- bashJson (searchConcept task)
symbols   <- lsp.resolveSearchHits hits
shortlist <- relevant task =<< source.describeSymbols symbols

candidate <- pick
  "Best existing implementation of the needed behavior"
  shortlist

definition <- source.read candidate.range
callers    <- lsp.incomingCalls candidate
usage      <- pick "Representative production use" callers
usageBody  <- source.read usage.range

tests   <- discoverTests [candidate, usage]
example <- pick "Test demonstrating the required edge case" tests
result  <- bash example.command

pure (ReuseEvidence definition usageBody example result)
```

The result contains an owning implementation, a real consumer and a tested example.
Candidate descriptions should include signatures and relevant source rather than
relying on names. No suitable mechanism or missing edge-case coverage becomes an
explicit gap, not a generated claim of reuse compatibility.

Traditional interaction compressed: conceptual search, symbol inspection, caller
inspection, test discovery and execution. The coding model receives evidence for
its reuse decision instead of manually navigating every intermediate result.

## 4. Resolve a compiler error through the type's history

Inquiry: “Find the intended migration behind this type mismatch.”

```haskell
build <- bash focusedBuild
err   <- pick task (compilerDiagnostics build)

expected <- lsp.typeDefinition err.expectedTypeLocation
actual   <- lsp.typeDefinition err.actualTypeLocation

history <- bashJson (historyFor [expected, actual])
change  <- pick
  "Change explaining why these two types now differ"
  history

patch <- bash (showCommit change)
uses  <- lsp.references expected
example <- pick
  "A current caller already migrated in the way this patch establishes"
  =<< source.describeReferences uses

pure (MigrationEvidence err patch example)
```

The final coding turn starts with the mismatch, a relevant historical change, and
a current migration example. Code handles Git identity, renames and source
versions. Historical intent is evidence to inspect, not an authority overriding
the current contract. Jev connects existing evidence; it invents neither a type
conversion nor a patch.

Traditional interaction compressed: inspect compiler output, resolve both types,
search history, inspect a commit, find and read a migrated caller.

## 5. Construct a minimal reproducer from existing tests

Inquiry: “Find the smallest existing test setup that reaches this suspicious branch.”

```haskell
target  <- lsp.definition suspiciousLocation
callers <- lsp.incomingCalls target

path <- walkBackwardsBounded 4 callers $ \candidates ->
  pick "Path most likely to reach this branch from a test" candidates

fixtures <- source.testFixtures path
fixture  <- pick
  "Smallest fixture preserving the preconditions of the suspicious branch"
  fixtures

baseline <- bash fixture.command
probe    <- pick
  "Which available diagnostic mode distinguishes the two suspected paths?"
  (diagnosticVariants fixture)

observed <- bash probe.command
pure (Reproduction fixture path baseline observed)
```

Diagnostic variants are authored command values: enable an existing trace mode,
select one test case, or use a supported feature flag. The fixture is minimal
among the offered candidates, not proven globally minimal. Call-graph reachability
does not prove runtime branch coverage; retain trace/coverage evidence or report
that reaching the branch remains unverified.

Traditional interaction compressed: inspect a symbol, walk callers, locate test
fixtures, run a baseline, select instrumentation and rerun with it.

## Shared design implications

These cells require fluent composition between command observations, code-tool
results and checked semantic candidate sets. A selection returns the original
typed payload, ready for the next operation. Different steps can use different
question schemas while sharing the one Jev effect operation.

Retain original observations, selected paths, distributions and exact source
identities behind compact results. A final frontier turn should receive useful
evidence or a specific unresolved decision. Tool support, runtime suspension,
process lifecycle, actor authority and source consistency stay with their existing
owners; these examples do not introduce a second execution framework.
