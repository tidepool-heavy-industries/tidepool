# Notebook architecture review

Review the recovered implementation against the running host, not only its
source-generation tests. The release boundary is a prepared cell with fixed
compiler identities and retained dependencies; executing it must not reconstruct
its meaning from a later session view.

## Findings and repairs in progress

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

5. **Classification and diagnostics have inconsistent owners.** Prepared items
   now reuse GHC's returned classification. The remaining resident line/block
   parser, prologue handling across check and execution, and metadata lost on
   whole-cell rejection still require consolidation. A single GHC-authored item
   plan must survive both success and failure. Rust should sequence and present
   that plan, not reclassify source or infer outcomes from error text.

## Remaining review gates

- Staging and committed declaration rendering currently repeat work. Consolidate
  their shared candidate construction without bypassing capture retention,
  value-name replacement, or durable recovery. Remove failed candidate artifacts
  at the owning source/artifact boundary; cleanup success must not authorize ID
  reuse.
- Check cell cancellation, terminal `respond`, and fork publication against the
  actual source-item receipt. An accepted declaration prefix is durable even if
  execution is then interrupted. Rejection before preparation commits installs
  nothing; ordinary effects remain nontransactional.
- Review `last` and display continuations as retained values, with one budget
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
import shadowing. Deterministic interleaving, full recovery, source-plan, display,
and combined-release checks remain open.
