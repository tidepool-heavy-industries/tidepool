# L4 spike — how the runtime invokes an author's handler closure

STATUS: DECIDED. Verified empirically before anything was built on it.

The question this lane could not start without: `withHandler ev handler body`
takes a closure the author wrote inline, in the ambient `M effs` row. Something
has to APPLY that closure to each observation. What?

**Answer: nothing in the runtime applies it. Haskell does.** `withHandler` is a
scoped INTERPOSITION over `body`'s own freer-simple structure. The closure never
leaves Haskell, no continuation is created for it, and `tidepool-codegen` does
not grow.

---

## 1. The two scaffolding findings, verified independently

Both hold. Neither was taken on faith.

**(1) `run_fragment_suspendable_parked` takes a `FuncId`, and there is no public
closure-apply on the parked path.** Confirmed by reading the signature
(`jit_machine.rs:3093`) and enumerating every public run entry on
`JitEffectMachine`: `run`, `run_suspendable{,_parked}`,
`run_fragment_suspendable{,_binding,_parked}`, `run_fragment{,_pure}`,
`run_pure{,_and_bind}`, `run_fragment_and_bind{,_projected,_render}`,
`run_child_fragment{,_pure}`, `resume_suspended{,_binding}`, `resume_parked`.
Not one of them accepts a closure `Value` to apply. Note also that the fragment
entries take NO argument vector — a `FuncId` fragment is entered nullary — so
even a hand-compiled dispatcher `FuncId` could not receive an observation as an
argument; it would have to fetch one through an effect. `resume_applied` and
`park_continuation` are private and §3-listed, so they are unavailable by rule
as well as by visibility.

**(2) There is no production consumer of the parked path.** A repo-wide grep for
`run_suspendable_parked` / `run_fragment_suspendable_parked` / `resume_parked`
returns hits in exactly six files, all of them
`tidepool-codegen/tests/realm_*.rs`. This lane's acceptance harness is the
seventh caller and the first outside `tidepool-codegen`.

## 2. The mechanism

`Eff` is freer-simple's free monad, and its constructors are reachable from
generated Haskell via `Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)`.
So a scope can WALK the computation it encloses and interpose an action before
every effect the body performs:

```haskell
pumped :: Eff effs () -> Eff effs a -> Eff effs a
pumped tick (Val a) = Val a
pumped tick (E u q) = tick >> E u (tsingleton (\x -> pumped tick (qApp q x)))
```

`withHandler` is that, with `tick` = "drain this subscription's queue, applying
the author's closure to each observation in order":

```haskell
withHandler ev handler body = do
  sub <- subscribeEvent (eventSources ev)          -- non-blocking; no replay
  r   <- pumped (drainSub ev handler sub) body     -- interposed for body's extent
  drainSub ev handler sub                          -- lexical drain, after intake closes
  unsubscribeEvent sub
  pure r
```

The runtime's whole job shrinks to answering three ordinary effect requests —
subscribe, drain, unsubscribe — plus keeping per-subscription queues. It never
holds a Haskell closure, never roots one, and never applies one.

### Why this satisfies each authored semantic

| Semantic | How it falls out |
|---|---|
| No replay to a fresh subscription | `subscribeEvent` returns a cursor at the journal's current end; the queue starts empty. |
| Broadcast, not consumed | Each subscription has its OWN queue; a reconciliation pass appends to every registered one. |
| One handler at a time per subscription | `pumped` recurses on `body` ONLY — the tick's own effects are not pumped by its own pump, so a subscription cannot re-enter its own handler. Structural, not enforced. |
| Later matches queue in observation order | The drain is a sequential `mapM_` over a FIFO. Haskell's own sequencing is the ordering guarantee. |
| Separate handlers interleave only at suspension points | Nested `withHandler` = nested `pumped`. An effect inside the inner handler is an effect, so the outer pump sees it — which is exactly "interleave at suspension points". Verified: nesting produces `B B A B B B A B`, the outer tick firing around the inner tick's own effects. |
| Handler may itself suspend | A suspension inside the tick is an ordinary suspension of the resident's own continuation. There is no separate handler continuation to reconcile. Verified end-to-end through a real `ask` suspend/resume round trip: the handler suspended mid-flight, resumed, and the body completed with the right value. |
| Lexical drain then unregister | Written literally in `withHandler`'s own body, after `pumped` returns. |
| Handler failure fails the enclosing scope | The tick is in the same `Eff`; its failure propagates by ordinary means. Verified: a failing tick aborted the eval and the statement after `withHandler` never ran. |
| Bounded queue, loud overflow | `drainSub` returns a typed drain result; an overflow marker becomes a loud failure in the enclosing scope rather than a dropped commit. |
| Registration does not block | `subscribeEvent` is a cheap registry insert. |

### Verification record

Four evals against the live JIT, in order:

1. `pumped` over a 2-effect body → tick fired exactly twice, body returned `7`.
   Proves the walk compiles and runs on the JIT (existential GADT match over
   `Eff`, type-aligned `Arrs` re-queued through `tsingleton`).
2. Nested pumps with an effectful inner tick → `B B A B B B A B`, result `7`.
3. Failing tick → eval aborted with the handler's error; the post-`withHandler`
   statement did not run.
4. Tick containing `ask` → real suspension, real resume, handler continued past
   its own suspension point, body completed.

## 3. Rejected alternatives, and why

**Grow `tidepool-codegen` with a closure-application entry point on the parked
path.** This was pre-authorized and is the alternative the design stance points
at. Rejected because it is not needed: the authored semantics are met in full
without it. Taking it would have added a public entry that must hold the
closure as a GC root across collections for the subscription's whole lifetime —
a new rooting obligation on the hardest surface in the system — to buy nothing
the interposition does not already give. The design stance says grow the runtime
when the runtime cannot serve the DSL; here it can.

**Compile a top-level dispatcher `FuncId` that reads a closure from a
Haskell-side table.** Rejected on a hard fact: fragment entries are nullary, so
the dispatcher cannot be handed the observation, and the closure would still
have to be stored somewhere Rust can root it and hand it back — reintroducing
the rooting obligation above, plus a second continuation per handler invocation
that then has to be reconciled with "one at a time per subscription".

**Let the driver resume the resident into the handler.** Rejected because
`resume_parked` feeds an answer to the effect the resident is parked ON. To make
it deliver "run this handler instead", every effect's answer type would have to
become a sum carrying a possible handler invocation — a token threaded through
the whole row, which is precisely the DSL-shrinking move the stance forbids. The
mechanism chosen achieves the same shape (the handler runs as ordinary
continuation of the resident's own code) without the token, because the
interposition happens in Haskell before the request is ever sent.

**Run the handler as a second parked continuation while `body` is parked.**
Rejected as unnecessary once the interposition is available, and expensive if
taken: it needs an entry point to create the continuation (see the two rejected
alternatives above) and would then need explicit machinery to guarantee
one-at-a-time-per-subscription, which the interposition gets structurally.

## 4. Consequences worth stating plainly

- **`tidepool-codegen` is untouched by this lane.** No root was notified for a
  codegen change because none is being made.
- **The parking contract is consumed, not extended.** The acceptance harness
  drives `run_suspendable_parked` / `resume_parked` directly, asserts
  `stowed_roots_count() == parked_count()` at every quiescent point, derives the
  handled prefix from the value that built the handler stack, and touches
  neither `suspended_continuation` nor the nested-child entries.
- **Cost, stated honestly:** the pump sends one drain request per effect the body
  performs, so a `withHandler` scope roughly doubles the body's effect count.
  That is a real overhead and a legitimate future optimization (batch the drain
  check, or interpose only on effects that can block). It is not a correctness
  issue and it buys the whole semantic set with no runtime change.
- **`Control.Monad.Freer.Internal` becomes an import of the generated
  `Tidepool.Effects`.** That is a new dependency on freer-simple's internal
  module. It is stable (the `Eff`/`Arrs` shape is a locked decision in the root
  `CLAUDE.md`) but it is a dependency, and it is the one thing here that would
  break if freer-simple were ever swapped.
