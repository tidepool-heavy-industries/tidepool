# Freer resume boundary

`program` is a real `freer-simple` `send` program whose second `Ask` request
depends on the first response: `a <- send (Ask 3); b <- send (Ask (a + 1))`.
The second request's argument is built from the first answer, so a handler
cannot answer it without actually resuming the first suspension (matches the
cohort probes' opacity requirement — operands are not observable independent
of each other).

`resumeInt k n = qApp k (I# n)` is the compiled resume wrapper from Wave 6B
decision D2: calling `resumeInt` from Rust with a managed retained
continuation (`k`, the `Arrs` queue captured inside the `E` constructor) and a
scalar `Int#` answer applies the continuation through the real compiled
`qApp`, with no Rust-side freer walker and no second decoder of the freer
envelope.

`askArgument :: Req Int -> Int#` and `valResult :: Eff '[Req] Int -> Int#`
(added for E2, the resume-loop test) are the engine's idiom for forcing a
value from Rust, used the same way as `resumeInt`: a tiny compiled Haskell
top, called through `run_entry_retained` with the retained value as a
`Managed` argument. They exist because `PreparedRuntime::inspect_outer`
(`tidepool-codegen/src/prepared_program/observe.rs`) is deliberately
observation-only — it reads one already-WHNF constructor layer and never
forces a field. `Union`'s payload field (`Data.OpenUnion.Internal`, a
library type this probe does not own) and `Val`'s boxed `Int` field are
both ordinary lazy fields, so the retained request/result are still `Thunk`
objects when E2's resume loop reaches them by `inspect_outer` alone;
`askArgument`/`valResult` are the ordinary pattern matches that force them,
run as compiled code exactly like every other top here — not a second
decoder of the freer envelope, and not a Rust-side freer walker (E2 still
finds `Union`'s tag/payload shape, and distinguishes `E` from `Val`, only
through `inspect_outer`). `Ask`'s own field is additionally marked strict
(`Ask :: !Int -> Req Int`, not the original `Ask :: Int -> Req Int`) so that
once `askArgument`'s pattern match reaches it, it is already unboxed to
`Int#` rather than one more field `inspect_outer` cannot reach; this changes
nothing about `program`'s two-dependent-`send`s shape or its opacity to the
simplifier — `Ask 3`'s literal argument and `Ask (a + 1)`'s computed one are
equally forced by the strict field, and the second `send` still cannot be
answered without actually resuming the first suspension.

`program`, `resumeInt`, `askArgument` and `valResult` do not reference each
other's top-level bindings, so the corpus projection's per-entry
reachability closure (`selectPreparedTarget` in
`Tidepool.ExecutionProjection`) keeps them apart under separate projection
targets — each projected alone admits only itself (and its own transitive
dependencies). `freerResumeEntries = (program, resumeInt, askArgument,
valResult)` is a fifth top whose only job is to reference all four by name,
so its own reachability closure pulls in all four as tops of one projected
`WireProgram`. That row (index 2, `2.prepared.cbor`) is the one copied into
`fixtures/freer-resume.cbor`; the other four appear as their own
(redundant, single-target) rows in the projection manifest, kept as target
lines for provenance and because each alone already answers whether it
survives recovery as an admitted top.

The fixture intentionally supplies no per-effect-handler expectation,
matching `FreerRetention`: it pins the artifact and the multi-entry
admission boundary without inventing an effect interpreter. `program`'s
own final `Int`, for the identity-answer policy E2's resume loop drives
(`Ask n` answered with `n` itself), is separately pinned in
`FreerResumeExpectations.json`, transcribed from `FreerResumeOracle.hs` run
under the pinned GHC — never hand-derived. `resumeInt`'s projected entry
signature has semantic arguments `[LiftedRef, Int(64)]` — the retained
continuation `k` and the unboxed `Int#` answer — with no `Void` state-token
argument, since `Eff` is not `IO`.
