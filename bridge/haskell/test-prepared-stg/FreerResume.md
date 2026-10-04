# Freer resume boundary

`program` performs two real `freer-simple` sends: `Ask 3`, then `Ask (a + 1)`
where `a` is the first answer. A handler must resume the first continuation
before the second request can be observed.

`resumeInt k n = qApp k (I# n)` applies a retained continuation through compiled
Haskell. `askArgument` and `valResult` force the request payload and settled
result through ordinary compiled pattern matches. The Rust observation boundary
reads one constructor layer; these helpers force lazy fields without adding a
second interpreter. `Ask` carries a strict `Int` field so its observed argument
is already unboxed after the helper reaches it.

`freerResumeEntries` references `program`, `resumeInt`, `askArgument` and
`valResult`, retaining all four in one prepared reachable closure. The declared
`//bridge/haskell:freer_resume_prepared` action compiles all five targets once.
Runtime tests read `freerResumeEntries.prepared.cbor` from the generated resource
directory. Fixture changes regenerate that resource independently of Rust
linking. No prepared artifact is checked into source.

The resume-loop test answers each `Ask n` with `n`; the independent native
`FreerResumeOracle.hs` evaluates that same handler policy. The `resumeInt` entry
accepts the retained lifted continuation and an `Int(64)` response, with no
state-token argument because `Eff` is not `IO`.
