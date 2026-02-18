# Deep Research: freer-simple Core Shapes After GHC -O2 Optimization

## Problem Statement

Tidepool compiles Haskell `Eff` (freer-simple) programs to a flat Core AST that a Rust evaluator/effect-machine interprets. The Rust side expects specific data constructor patterns:

- **`Val(x)`** — pure return (terminal state)
- **`E(Union(tag, req), k)`** — effect request with continuation
- **`Leaf(f)`** — continuation leaf (single closure)
- **`Node(k1, k2)`** — continuation composition

The effect machine (`core-effect/src/machine.rs`) pattern-matches on these constructors by `DataConId` to drive the coroutine loop. If GHC's optimizer transforms these patterns into something structurally different, the machine will fail at runtime.

We need to understand exactly what Core shapes `freer-simple` produces at `-O2` (with `Opt_FullLaziness` disabled) for a realistic program.

## Source Program

```haskell
{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Guess where

import Control.Monad.Freer

data Console a where
  Emit     :: String -> Console ()
  AwaitInt :: Console Int

emit :: Member Console effs => String -> Eff effs ()
emit = send . Emit

awaitInt :: Member Console effs => Eff effs Int
awaitInt = send AwaitInt

data Rng a where
  RandInt :: Int -> Int -> Rng Int

randInt :: Member Rng effs => Int -> Int -> Eff effs Int
randInt lo hi = send (RandInt lo hi)

game :: Eff '[Console, Rng] ()
game = do
  target <- randInt 1 100
  emit "I'm thinking of a number between 1 and 100."
  guessLoop target

guessLoop :: Int -> Eff '[Console, Rng] ()
guessLoop target = do
  emit "Your guess? "
  guess <- awaitInt
  if guess == target
    then emit "Correct!"
    else do
      emit (if guess < target then "Too low!" else "Too high!")
      guessLoop target
```

## Research Questions

### Q1: What Does `send` Compile To?

`send` from freer-simple has type:
```haskell
send :: Member eff effs => eff a -> Eff effs a
```

After inlining and optimization, what Core does `send (Emit "hello")` become?

Specifically:
- Does it produce `E (Union tag# (Emit "hello")) (Leaf (\x -> Val x))`?
- Or does GHC inline/rewrite the `Member` dictionary and `Union` construction into something different?
- Does the `Union` constructor survive optimization, or does GHC see through the newtype/GADT?
- What is the `tag#` value — is it a literal `Word#` (`0##`, `1##`) or a computed expression?
- Does `Leaf` survive, or does GHC optimize `Leaf (\x -> Val x)` into something else?

### Q2: What Does Monadic Bind (`>>=`) Compile To?

freer-simple's `>>=` builds `FTCQueue` via `Node`:
```haskell
instance Monad (Eff effs) where
  Val x >>= f = f x
  E u q >>= f = E u (q |> f)  -- snoc onto the queue
```

After optimization:
- Does `>>=` get inlined completely?
- Does `|>` (snoc on `FTCQueue`) get inlined to `Node(q, Leaf(f))`?
- Or does GHC produce a different representation?
- For a chain like `randInt 1 100 >>= \target -> emit "..." >>= \_ -> guessLoop target`, what is the final Core shape?

### Q3: What Does the Top-Level `game` Binding Look Like?

After `core2core`, what is the structure of the `game` binding?

Expected (if naive):
```
game = E (Union 1## (RandInt (I# 1#) (I# 100#)))
         (Node (Leaf (\target -> E (Union 0## (Emit "I'm thinking..."))
                                   (Node (Leaf (\_ -> guessLoop target))
                                         (Leaf (\x -> Val x)))))
               (Leaf (\x -> Val x)))
```

But GHC might:
- Float out sub-expressions (`game_s14e = I# 1#`, etc.)
- Share continuation closures across call sites
- Inline `guessLoop` (or not — it's recursive)
- Eta-expand or eta-reduce continuations
- Specialize `Member` dictionaries differently

What does it ACTUALLY look like? Please provide the full Core dump (or a faithful reconstruction) for the `game` binding after `-O2`.

### Q4: Union Tag Representation

freer-simple uses type-level lists (`'[Console, Rng]`) and `Member` typeclass to compute union injection tags. After optimization:

- Are the Union tags literal `Word#` values (`0##` for Console, `1##` for Rng)?
- Or are they computed from dictionary evidence at runtime?
- Does GHC specialize `Member` instances fully at `-O2` so that tags become static?
- What is the exact Core for `send (RandInt 1 100)` in the context of `Eff '[Console, Rng]`?

This matters because the Rust effect machine dispatches on the tag as a plain `u64`:
```rust
fn dispatch(&mut self, tag: u64, request: &Value, table: &DataConTable)
    -> Result<Value, EffectError> {
    if tag == 0 { self.head.handle(...) }
    else { self.tail.dispatch(tag - 1, ...) }
}
```

### Q5: FTCQueue Structure After Optimization

`FTCQueue` from freer-simple is:
```haskell
data FTCQueue m a b where
  Leaf :: (a -> m b) -> FTCQueue m a b
  Node :: FTCQueue m a x -> FTCQueue m x b -> FTCQueue m a b
```

After optimization:
- Do `Leaf` and `Node` constructors survive in the Core output?
- Does GHC ever "look inside" a GADT constructor and rewrite its contents?
- For `q |> f` (which is `Node q (Leaf f)`), does this stay as `Node q (Leaf f)` in Core?
- Are there any cases where GHC might produce a `Leaf` containing something other than a lambda?

### Q6: Recursive Bindings and Effect Structure

`guessLoop` is recursive. After optimization:
- Is `guessLoop` a separate top-level binding, or does GHC inline it into `game`?
- If separate, how does the continuation reference it? (Direct `Var` reference to the binding)
- If GHC performs worker/wrapper on `guessLoop`, does the wrapper preserve the `E(Union, FTCQueue)` structure?
- Does the recursive call `guessLoop target` in the `else` branch produce a thunk or get evaluated eagerly?

### Q7: Case-of-Known-Constructor Optimization

GHC's simplifier aggressively applies case-of-known-constructor. For freer-simple:
- Does `case E u q of { Val x -> ...; E u' q' -> ... }` get simplified?
- Since the scrutinee is `E u q` (a known constructor), does GHC eliminate the case and inline the `E` branch directly?
- This would mean the `>>=` implementation mostly vanishes, leaving just direct constructor applications. Is this what happens?

### Q8: Worker/Wrapper and Unboxing

Does GHC attempt to unbox any freer-simple types?
- `Union` wraps an `Any` — does worker/wrapper try to unbox this?
- `FTCQueue` is a GADT — does strictness analysis see through it?
- `Eff` is `Free (Union r)` — does GHC specialize `Free` for the specific `Union r`?

## Environment

- **GHC**: 9.12.2
- **Optimization**: `-O2` with `gopt_unset ... Opt_FullLaziness`
- **freer-simple**: Latest Hackage version compatible with GHC 9.12 (may need MonadBase patch)
- **Pipeline**: `parseModule → typecheckModule → hscDesugar → core2core`

## Expected Output

1. **Actual Core dump** (or faithful reconstruction) of `game` and `guessLoop` after `-O2`
2. For each question, explain what GHC does and doesn't optimize, with references to GHC simplifier source
3. **Inventory of constructors** that the Rust effect machine needs to handle — is `{Val, E, Union, Leaf, Node}` sufficient, or does optimization introduce other shapes?
4. **Known gotchas** where GHC optimization breaks the expected constructor pattern, and workarounds (e.g., NOINLINE pragmas, specific optimization flags to disable)
