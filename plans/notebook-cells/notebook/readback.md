# Notebook lane execution readback

Source: `0db96b2b6979e369a44e62da45fdde3f297e56b6`

## Verified present boundaries

* `haskell/src/Tidepool/Binders.hs` is the GHC parser owner.
  `classifyWithFlags` tries declaration, statement, and headerless-module
  parsers; `classifyBlock` shares one GHC session for a batch. Its result has
  `kind`, binders, and structured declaration exports.
* `tidepool-runtime/src/session/turn.rs` owns the Rust invocation and strict
  decoding of that result. `run_turn` compiles one selected template.
  `GhcPipeline.capturedBindingType` obtains a bind wrapper's result `Type`;
  `capturedTopLevelTypes` only renders compiler-reserved top-level probes.
  Neither walks binders inside a typechecked `do`.
* `tidepool-runtime/src/session/workbench.rs` owns today's Pest split and
  `WorkbenchRequest::from_ghci_input`. `WorkbenchItemReceipt` has an index but
  no kind/span; `WorkbenchResponse` is already one response containing items.
  `WorkSequence` and `run_block_sequence` own ordered committed prefixes.
* `tidepool-actor/src/resident_workbench.rs` is the real consumer. It compiles
  and executes one `ParsedBlock` at a time. A declaration is first parsed by
  `run_turn`, then semantically installed by `define_scoped_with_imports_in`.
  Expressions are compiled as uniquely named observation binds and display
  immediately. Truncation currently suggests `inspectFull (observationN ())`.
* `PersistentSession`/`ResidentSession` already own `Lib.G` declaration
  generations, `Val.G` thin interfaces, incarnation-aware declaration heads,
  scoped commits, and recovery. Cell work must compose these APIs.
* Display lives at `haskell/lib/Tidepool/Inspection.hs`, not the path in the
  initial proposal. Its `Display` fallback requires `Show`; it returns only
  `(Text, Bool)`, so it has neither a rendering tree nor continuation.
  `EVAL_PRAGMAS` contains `DeriveGeneric`/`DeriveAnyClass` but not
  `StandaloneDeriving`. Automatic `Generic` alone cannot meet the display
  promise.

## First shared scaffold

The parent will own and land the wire before implementation fanout:

```text
CellAnalysis
  declarations: rewritten source + exports
  items: CellItem { sourceSpan, kind, source, binders, inferredBinderTypes }
  diagnostics: [diagnostic tied to sourceSpan]
```

The extractor receives the original cell and current synthesized scope in one
request. It lexes/splits, classifies, builds one check module containing the
declaration group and one `do`, and returns no executable artifacts. Rust never
classifies Haskell. Checking is non-mutating. On success the actor installs the
returned declaration source as one existing `Lib.G` step, then compiles/runs
statements and observations sequentially using the checked type information.
Receipts preserve both source-item ordinal/span and execution-step grouping.

The exact transport for inferred types remains deliberately unsettled. Printed
types are insufficient until they are proved re-parseable in the post-declaration
scope. The worker result should therefore not freeze a `String`-only public
contract before the feasibility probe.

## Load-bearing feasibility findings

The suggested `let limit = 3` baseline is not a failing regression here:
the live workbench reports `limit :: Int`. It cannot serve as the lane's
falsifier. Likewise `x <- pure Nothing` installs as
`x :: forall {a}. Maybe a`; statement separation sometimes generalizes rather
than rejects. The real risk is agreement and sound identity, not uniformly
stricter inference.

The checked probe in `evidence/inference-transport.md` now establishes two
actual GHC boundaries. A `read` binder fixed downstream is accepted in one
`do` but rejected as ambiguous when compiled alone. A fresh Astra GHC 9.12.2
API probe walks `tm_typechecked_source` after zonking and observes both binder
and use as `x :: Maybe G`, where `G` is declared in the same synthesized
module. Explicitly pinning a staged bind to an installed user type also
compiles and is consumed successfully.

The remaining blocker is faithful automatic transport: the synthesized
module's nominal `G` is not the later installed `Lib.G<n>.G`, and no current
wire relocates a GHC `Type`. Printed/reparsed syntax has not been shown correct
for shadowing, hidden names, constraints, or skolems. Before the wire is
frozen, the parent must extend the probe through the owning resident consumer:

1. a `Response` whose result is fixed downstream to a type declared in the same
   cell, installed and consumed after staging;
2. a shadowed prior declaration of the same name;
3. retained constraints/polymorphism, and a negative type mentioning a local
   skolem or otherwise non-replantable name.

The same-cell `G` observation proves harvesting, not general transport. If any
checked binder type cannot be represented against the installed declaration
head, cell preflight rejects before declaration installation or user effects.

The Astra consultation returned `NeedEvidence`: harvesting is feasible, but
transport is not established. It owns the eventual decision between post-zonk
AST harvesting plus scope-safe replanting and a different representation. If harvest-and-pin cannot
preserve the promised inference, staged-shape checking is sound but is a product
degradation and must be escalated. Stage 2 is not a fallback implementation in
this wave.

## Recursive implementation tree

After planner release and the shared analysis fixture:

```text
notebook lead (retains analysis/execution agreement and hosted actor join)
├─ extractor cell frontend
│  ├─ GHC lexer split/layout/span fixtures
│  └─ synthesis, diagnostics, inferred-binder transport
├─ actor/runtime sequencing
│  ├─ request/receipt types, preflight, committed prefix
│  └─ Lib/Val installation, shadowing, recovery, terminal respond
└─ display and declared types
   ├─ Generic eligibility/generic Display including opaque functions
   └─ rendering tree, latest-display `last`, typed paging continuation
```

The parent integrates each checked child against
`ResidentActorRunner::run_workbench_step`, owns cross-child repairs, and forks a
second local wave for corpus/hosted acceptance only after that consumer works.
Children may further unfold after establishing their local interface. Shared
tool registration and common `documentation_tests` wiring remain coordinator
owned; this lane supplies notebook fixtures and final cell-facing descriptions.

## Decisions and open questions

* Keep declaration capture of same-cell statement bindings rejected: the
  `Lib.G` module boundary owns this invariant.
* Keep `respond` terminal with the tail `NotRun`. Do not statically reject a
  middle `respond`; examples place it last.
* `last` is changed only by a successful display; rejected/no-display cells
  leave it unchanged. Before implementation, the display child must show the
  exact types of `last` and `last.more`, whether repeated expansion advances,
  and how multiple expression displays share a cell budget. A fresh Astra
  consultation resolves any consequential typing tradeoff.
* Generic eligibility must come from parsed declarations. Existing `Generic`
  deriving, GADTs, existentials, parameters, and function fields each need a
  fixture. The implementation must supply generic `Display`, not merely derive
  `Generic`.
* GHC-lexer fixtures must include quasiquotes and multiline strings containing
  blank and column-one lines, nested comments, pragmas, and a declaration with
  a hanging `where`. Existing Pest behavior is comparison evidence, not the new
  implementation.

## Checks

Baseline coordinator evidence: extractor-aware `cargo build --workspace`
completed. The broad `just quick` attempt was killed with exit 137 while starting
1,626 tests and is not acceptance evidence.
`just test-lib tidepool-runtime
'test(=session::workbench::tests::ghci_script_preserves_multiline_quasiquotes_and_following_units)'`
executed one current-parser comparison test and passed.

First implementation checkpoint must make a currently unsupported cell analysis
fixture pass, including the downstream-fixed monadic binder and cell-owned nominal
type. Final proof is the hosted tool: mixed cell; late type error runs nothing;
runtime failure commits declaration plus preceding values; next cell sees them;
middle `respond` marks its tail `NotRun`; recovery and shadowing retain identity.
