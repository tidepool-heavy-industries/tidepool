# Notebook architecture review

Review the recovered implementation against the running host, not only its
source-generation tests. The release boundary is a prepared cell with fixed
compiler identities and retained dependencies; executing it must not reconstruct
its meaning from a later session view.

## Findings and repairs

1. **Preparation released the session before claiming its identities.** Separate
   check, stage, and execute checkouts let another actor allocate declaration or
   value generations used by the prepared code. Whole-cell checking and staging
   now share one checkout. The declaration prefix commits through the existing
   declaration owner before that checkout ends. Value identities are claimed
   before compiler invocation, including failed invocations, because the worker
   may write an interface before reporting failure. Effects still release the
   machine normally; this does not serialize actors across an effect wait.

2. **Checked types were transported as ambiguous occurrence names.** A later
   definition of `Version` made generated signatures confuse old retained values
   with the new type. GHC now qualifies session type heads with their defining
   generation. The staged compile view masks shadowed unqualified names while
   keeping qualified access and exact value interfaces. The declaration checker
   uses the actual candidate module identity rather than a throwaway nominal
   identity. No Rust text substitution attempts to relocate Haskell types.

3. **Prepared Core had no dependency lifetime.** Later code could reference an
   automatic observation evicted by earlier displays. Prepared work now leases
   referenced binding IDs through `BindingTable`, including future bindings.
   Fork tips and prepared work use the same lease accounting. Cancellation drops
   the lease through the runtime's existing custody cleanup queue; the next
   mutable session entry or teardown reclaims it. Execution can still promote
   explicit captures to ordinary binding lifetime through the existing owner.

4. **Whole-cell expression checking did not infer effect rows.** An instance
   specialized to the exact row overlapped the pure-value instance before GHC
   could infer an action's row. The action instance now matches `Eff effects a`
   and imposes equality with the actor's row as a constraint. Bare `pollResponse`
   works without an authored row annotation; wrong rows remain rejected.

5. **Classification and diagnostics had inconsistent owners.** Prepared items
   reuse GHC's returned classification. Rejected cells now retain the complete
   source-item plan, with individual diagnostics attached by original spans.
   GHC now separates flags, imports, and
   declaration bodies; Rust renders those typed fragments. Compiler options are
   cell-local, but retained with compiled declarations for recovery. Imports
   commit with the declaration group. The old resident line/block parser and its execution bypass have been removed;
   the standalone operator REPL retains only its own command tokenization.

6. **The whole-cell check compiled twice.** The second pass only checked the
   rendered binder signatures. Each statement already checks those signatures
   in its actual value module before commit, so the duplicate whole-cell compile
   has been removed. Declaration preparation also reuses the checked receipt
   rather than asking GHC to classify the same declaration again.

## Review boundaries

- Validated staged declarations now adopt through the existing declaration owner
  with session, scope, generation, and visible-value fences. Focused real-GHC
  tests prove recovery recording and stale/committed artifact retention. Successor
  recovery also passes with both old and shadowing nominal types.
- Check cell cancellation, terminal `respond`, and fork publication against the
  actual source-item receipt. An accepted declaration prefix is durable even if
  execution is then interrupted. Rejection before preparation commits installs
  nothing; ordinary effects remain nontransactional.
- Review `cellDisplay` and display continuations as retained values, with one budget
  owner. Paging must not rerun an effect. Captured values and continuation
  dependencies need the same lifetime rules as other persistent bindings.
- Keep runtime facts in `status`, compiler facts in `lookup`, and source in
  `haskell`. Check both hosted argument gates and actual result representations,
  rather than only advertised schemas.
- Verify frozen-library Generic derivation and source flags through the same
  compiler path used in production, including unsupported field types and
  explicit instances. Do not implement a second Haskell parser in Rust.
- Corpus examples are acceptance inputs. Rewrite them to cells only when the
  implemented semantics support them; preserve minimal negative test fixtures.

## Evidence

The focused hosted fixtures pass for prefix/rejection/shadowing, retained old and
new nominal types through qualified Map imports, inferred Response results and a
stored action reused in a later cell, terminal reply, and observation dependencies
surviving nine earlier displays. Binding-owner and runtime tests cover future
binding leases, shared fork/prepared leases, dropped-result cleanup, and staged
import shadowing. Source-plan, nominal recovery, display, and the combined
fixture/overlay checks also pass; the release checklist records their results.
A dedicated deterministic cancellation-at-every-item interleaving test was not
run; cancellation relies on the existing checkout and custody owners.


## Display integration

The renderer now retains a lazy tree suffix, with an 8192-character allowance
shared across a cell's expression items. Custom legacy `displayWith` renderers
receive the remaining allowance; without `displayTree` they explicitly report
unavailable detail rather than pretending to supply a cursor. Built-in
containers, text, Show-backed values, response observations, and command values
have structural rendering paths. Focused pure traversal checks pass, including
infinite input and an unforced suffix. Hosted text, custom/automatic displays, and retained command paging pass.
The page stores its continuation behind a closure: capture must not recursively
materialize future pages.

`cellDisplay` publishes through the existing binding table as a scope-local alias of a
captured page. The alias keeps its source's dependency graph live, shares its
registered root, and expires when replaced and uncaptured. A child retains IDs
needed by inherited compiled code, but does not inherit the parent's alias name.
Focused alias lifetime, scope and lease validation tests pass.

Publication occurs after each successful display as a prefix commit. Every
item in the cell was compiled before execution against the previous `cellDisplay`
identity, retained by the preparation lease. Thus later items still see the
previous cell's page while cancellation retains the newest completed display;
no separate pending-publication state machine is needed. Hosted lexical, runtime-prefix, and inherited-child boundaries pass. Nominal
declaration recovery passes; asynchronous cancellation retains the synchronous
prefix publication contract and the existing dependency-lease cleanup owner.

GHC constraint solvability drives generated field rendering. The reusable API
and its scope requirements are recorded in
[compiler constraint queries](../../docs/compiler-constraint-queries.md).

Eligibility review found that H98 syntax alone does not guarantee a Generic
instance: a rank-n field or unsupported primitive representation can make valid
source fail during automatic derivation. Structural Display selection is now
separate from Generic eligibility, retaining opaque fields without rejecting
a valid authored declaration. The focused compiler and hosted paths pass. GHC
structured eligibility diagnostics remove only unsupported generated instances;
explicit invalid deriving remains an authored error.

Observation recency must follow completion order, not compilation generation.
A notebook cell precompiles its expressions; pages compiled during execution can
have newer generations than a later expression. Sorting by generation evicted a
new ninth result immediately. The binding owner now orders the bounded window
by save order, with a regression test for out-of-order compiled generations.
The hosted nine-expression/captured-page integration test passes.

Effectful observations conservatively preserve their referenced observations
until scope retirement because an effect can export slot-dependent closures.
This existing escape-safety rule also applies to `cellDisplay.more`: repeated effectful
paging may retain earlier page dependencies for the scope lifetime. Pure page
alias replacement remains collectable. Narrowing effectful retention requires
tracking actual escapes through effect owners; recognizing the spelling
`cellDisplay.more` would not make it safe.

Fresh instance-identity review removed parser-based class-name matching.
Structured GHC duplicate-class and builtin Rep-family diagnostics now preserve
true explicit instances, including aliases and reexports. Unrelated classes named
Display or Generic cannot suppress generation. Specialized custom Display
instances coexist with the generated structural instance. Focused regressions pass.

The display binding is `cellDisplay`. Its default belongs to the common actor
source imports alongside `print`, so whole-cell checking and declaration staging
see the same bindings. A declared `emit x = print x` helper exercises this path. Existing value aliases shadow it through the normal
scope projection. No Prelude `last` replacement remains. Hosted inherited-child
checks cover both fresh authored code and a retained parent closure.
