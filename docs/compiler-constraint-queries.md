# Asking GHC whether a capability is available

The extractor can ask GHC whether a class constraint is solvable in the current
module, with the same instances and local givens as generated code. This is a
reusable mechanism for choosing typed adaptations. Keep it in the Haskell
compiler owner; Rust consumes the resulting decision.

For example, a generated renderer can ask whether `Display fieldType` holds.
An imported type with a custom `Display` instance uses that instance. A type
with only `Show` can satisfy the Show-backed `Display` instance. A field with
neither can be rendered opaque without evaluating it. Looking up a matching
instance head alone cannot answer this: its context may itself be unsatisfied.

## GHC 9.12.2 entry points

The APIs verified against the repository toolchain are:

```haskell
initTcWithGbl
  :: HscEnv -> TcGblEnv -> RealSrcSpan -> TcM r
  -> IO (Messages TcRnMessage, Maybe r)

tcCheckGivens
  :: InertSet -> Bag EvVar -> TcM (Maybe InertSet)

tcCheckWanteds
  :: InertSet -> ThetaType -> TcM Bool

newEvVars :: TcThetaType -> TcM [EvVar]
mkClassPred :: Class -> [Type] -> PredType
```

Use the `TcGblEnv` from `tm_internals_` after typechecking, and the exact
`HscEnv` for that module. `Tidepool.GhcPipeline` owns this compiler phase.
Construct a predicate with the resolved class identity and GHC `Type` values.
For generated code with context `theta`, create evidence with `newEvVars theta`,
add it through `tcCheckGivens emptyInert`, and check the wanted predicate with
`tcCheckWanteds` under the returned inert set. The result tests the complete
constraint, including instance contexts. Quantified parameters remain rigid;
replacing them with inference metavariables would answer a different question.

The implementation of `tcCheckWanteds` checks `isSolvedWC` after solving;
see [GHC's constraint solver](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Tc/Solver.hs).
Use its typed result rather than parsing a diagnostic string. Failure to
initialize the checker or an unexpected compiler exception is an infrastructure
error, not evidence that the capability is absent.

## Generating code from an answer

The environment being queried must match the code emitted. For mutually
recursive generated renderers, provisional instance heads must be present
together before querying fields. Their provisional bodies can render opaque.
After selecting field renderers, typecheck the final generated bodies; only
that final version may reach extraction. Preserve the selected source through
staging and recovery instead of independently repeating the decision there.

This proves availability of typeclass evidence in one compiler scope. It does
not prove totality, execution cost, runtime authority, or a function's behavior.
A custom renderer may still diverge; an effect dictionary does not authorize a
resource. Those contracts stay with their existing owners.

## Other possible consumers

These are applications to evaluate when a production consumer needs them:

- Select a typed serializer, decoder, or schema adapter for a value.
- Offer operations in lookup or typed-hole suggestions whose constraints are
  actually satisfiable in the current scope.
- Choose a specialized presentation or an explicit opaque fallback without
  requiring every user type to implement every optional capability.
- Check an adapter's effect constraints against the current row before offering
  it as callable; runtime grants still decide whether a concrete action is allowed.

Reuse compiler evidence queries for these decisions rather than adding a
parallel registry of which type names are believed to support which operations.
