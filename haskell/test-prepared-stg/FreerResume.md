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

`program` and `resumeInt` do not reference each other's top-level bindings,
so the corpus projection's per-entry reachability closure
(`selectPreparedTarget` in `Tidepool.ExecutionProjection`) keeps them apart
under separate projection targets — each of `program` and `resumeInt`
projected alone admits only itself (and its own transitive dependencies).
`freerResumeEntries = (program, resumeInt)` is a third top whose only job is
to reference both by name, so its own reachability closure pulls in both
`program` and `resumeInt` as tops of one projected `WireProgram`. That row is
the one copied into `fixtures/freer-resume.cbor`; `program` and `resumeInt`
also appear as their own (redundant, single-entry) rows in the projection
manifest, kept as target lines for provenance and because either row alone
already answers whether `qApp` survives recovery as an admitted top.

The fixture intentionally supplies no expectation, matching `FreerRetention`:
it pins the artifact and the two-entry admission boundary without inventing
an effect interpreter. `resumeInt`'s projected entry signature has semantic
arguments `[LiftedRef, Int(64)]` — the retained continuation `k` and the
unboxed `Int#` answer — with no `Void` state-token argument, since `Eff` is
not `IO`.
