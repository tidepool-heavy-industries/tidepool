# Deep Research: Effect Machine Continuation Semantics vs freer-simple

## Problem Statement

Tidepool's `EffectMachine` (in `core-effect/src/machine.rs`) interprets freer-simple `Eff` programs by pattern-matching on `Val`/`E` constructors and walking `Leaf`/`Node` continuation trees. The semantics must exactly match freer-simple's Haskell implementation, or effects will be dispatched incorrectly, continuations will be applied wrong, or the program will diverge.

This research asks: does our Rust effect machine correctly implement freer-simple's operational semantics?

## freer-simple's Haskell Semantics

### Core Types

```haskell
-- The Eff monad (simplified)
data Eff (effs :: [* -> *]) a where
  Val :: a -> Eff effs a
  E   :: Union effs x -> FTCQueue (Eff effs) x a -> Eff effs a

-- Type-aligned fast append queue
data FTCQueue m a b where
  Leaf :: (a -> m b) -> FTCQueue m a b
  Node :: FTCQueue m a x -> FTCQueue m x b -> FTCQueue m a b

-- Tagged union of effects
data Union (effs :: [* -> *]) a where
  Union :: Word -> a -> Union effs a
  -- Note: the Word is the effect's index in the type-level list
```

### Bind Implementation

```haskell
instance Monad (Eff effs) where
  return = Val
  Val x >>= f = f x
  E u q >>= f = E u (q |> f)

-- |> is snoc: appends to the right of the queue
(|>) :: FTCQueue m a x -> (x -> m b) -> FTCQueue m a b
q |> f = Node q (Leaf f)
```

### Handler Implementation (run pattern)

```haskell
-- Simplified pattern for running one effect layer
run :: Eff '[e] a -> (a -> b) -> (forall x. e x -> (x -> Eff '[e] a) -> b) -> b
run (Val x) kp _  = kp x
run (E u q) _  kf = case decomp u of
  Right ex -> kf ex (\x -> qApp q x)
  Left _   -> error "impossible"

-- qApp applies the continuation queue to a value
qApp :: FTCQueue (Eff effs) a b -> a -> Eff effs b
qApp q x = case tviewl q of
  TOne f    -> f x
  f :| rest -> case f x of
    Val y -> qApp rest y
    E u q' -> E u (q' `tappend` rest)
```

### Critical Semantics: Queue Application

The key operation is `qApp` which uses `tviewl` to decompose the queue:

```haskell
data ViewL m a b where
  TOne  :: (a -> m b) -> ViewL m a b
  (:|)  :: (a -> m x) -> FTCQueue m x b -> ViewL m a b

tviewl :: FTCQueue m a b -> ViewL m a b
tviewl (Leaf f)     = TOne f
tviewl (Node l r)   = go l r
  where
    go :: FTCQueue m a x -> FTCQueue m x b -> ViewL m a b
    go (Leaf f)     r = f :| r
    go (Node l1 l2) r = go l1 (Node l2 r)  -- rotate left
```

This is a **left rotation** that ensures O(1) amortized access. The Rust side MUST implement the same rotation or else continuation application will be wrong.

## Rust Effect Machine Implementation

```rust
// Simplified from core-effect/src/machine.rs
fn run(&mut self, expr: &CoreExpr, handlers: &mut H) -> Result<Value, EffectError> {
    let mut current = eval(expr, &env, self.heap)?;
    loop {
        let forced = force(current, self.heap)?;
        match forced {
            Value::Con(id, fields) if id == self.val_id => {
                return Ok(fields[0].clone());
            }
            Value::Con(id, fields) if id == self.e_id => {
                let union_val = force(fields[0].clone(), self.heap)?;
                let k = force(fields[1].clone(), self.heap)?;
                // Extract tag and request from Union
                let (tag, request) = destructure_union(union_val)?;
                let response = handlers.dispatch(tag, &request, self.table)?;
                current = self.apply_cont(k, response)?;
            }
        }
    }
}

fn apply_cont(&mut self, k: Value, arg: Value) -> Result<Value, EffectError> {
    let forced = force(k, self.heap)?;
    match forced {
        Value::Con(id, fields) if id == self.leaf_id => {
            self.apply_closure(fields[0].clone(), arg)
        }
        Value::Con(id, fields) if id == self.node_id => {
            let k1 = fields[0].clone();
            let k2 = fields[1].clone();
            let result = self.apply_cont(k1, arg)?;
            let forced_result = force(result, self.heap)?;
            match forced_result {
                Value::Con(vid, vfields) if vid == self.val_id => {
                    self.apply_cont(k2, vfields[0].clone())
                }
                Value::Con(eid, efields) if eid == self.e_id => {
                    let union_val = efields[0].clone();
                    let k_prime = efields[1].clone();
                    let new_k = Value::Con(self.node_id, vec![k_prime, k2]);
                    Ok(Value::Con(self.e_id, vec![union_val, new_k]))
                }
            }
        }
    }
}
```

## Research Questions

### Q1: Does `apply_cont` Implement `qApp` Correctly?

freer-simple's `qApp` uses `tviewl` to left-rotate the queue before applying. Our Rust `apply_cont` recursively applies `k1` then feeds the result to `k2`.

- Are these semantically equivalent?
- `qApp` with `tviewl` does: extract the leftmost `Leaf`, apply it, then either feed to remainder (if Val) or thread remainder into continuation (if E).
- Our Rust code does: recursively apply left child, then handle result.
- **Potential issue**: Our recursive `apply_cont(k1, arg)` on `Node(k1, k2)` will recurse into nested Nodes without rotation. For a deeply right-nested tree like `Node(Node(Node(Leaf(f), Leaf(g)), Leaf(h)), Leaf(i))`, the Haskell `tviewl` rotates to get `f` in O(1), but our Rust code recurses all the way down. Is this just a performance issue, or can it produce wrong results?
- **Critical**: Can the recursion pattern cause stack overflow for deeply nested continuations?

### Q2: Effect Threading in Node

When `apply_cont(Node(k1, k2), arg)` encounters `E(union, k')` from applying `k1`:
- We construct `E(union, Node(k', k2))` — this threads `k2` onto the continuation.
- freer-simple's `qApp` does `E u (q' \`tappend\` rest)` — this appends the rest of the queue.
- Is `Node(k', k2)` equivalent to `q' \`tappend\` rest`?
- `tappend` is just `Node` in freer-simple, so yes. But verify there are no edge cases.

### Q3: Union Destructuring

freer-simple's `Union` is:
```haskell
data Union (r :: [* -> *]) a where
  Union :: {-# UNPACK #-} !Word -> a -> Union r a
```

After GHC optimization:
- Is the `Word` field unboxed to `Word#`? If so, in Core it appears as a raw `Word#` literal, not a boxed `W# x`.
- Our Rust side extracts the tag from `Union` by pattern-matching on `Con(union_id, [tag, request])`. But if GHC unboxes the `Word` field, the Core for `Union` construction might be `Union tag# request` where `tag#` is an unboxed `Word#`.
- **Critical question**: After serialization through our FlatNode format, does an unboxed `Word#` argument to a constructor get encoded as a `Lit` node or as a `Con(W#, [lit])` node?
- In the Translate.hs code, `collectArgs` + saturated constructor detection should handle this, but does it correctly handle unboxed fields?

### Q4: The `decomp` vs Linear Dispatch Issue

freer-simple uses `decomp` to peel off the first effect:
```haskell
decomp :: Union (e ': r) a -> Either (Union r a) (e a)
decomp (Union 0 a) = Right (unsafeCoerce a)
decomp (Union n a) = Left (Union (n-1) a)
```

Our Rust dispatch uses the tag directly:
```rust
if tag == 0 { self.head.handle(...) }
else { self.tail.dispatch(tag - 1, ...) }
```

- This is semantically equivalent to `decomp`. But does our code handle the **tag decrement** correctly? After peeling off one handler, the remaining handlers see `tag - 1`.
- What if GHC's optimizer changes the tag numbering? (See research/05)
- Is the tag 0-indexed (Console = 0, Rng = 1) or 1-indexed?

### Q5: Closure Application in the Evaluator

`apply_closure` evaluates a `Leaf`'s function applied to the response value. This goes through the tree-walking evaluator.

- When a `Leaf(f)` closure is applied to a response value `v`, the evaluator:
  1. Looks up the closure's body expression
  2. Extends the environment with the lambda binder → `v`
  3. Evaluates the body
- The body is typically something like `Val(v)` or `E(Union(...), ...)` or another bind chain.
- **Question**: Can the body be a `case` expression or other complex form? If GHC inlined aggressively, the continuation body might contain arbitrary Core, not just constructor applications.

### Q6: Thunk Handling in Continuations

Are continuation fields (`k1`, `k2` in `Node`, `f` in `Leaf`) thunked or evaluated?

- If the evaluator creates thunks for let-bound values, could `k1` or `k2` be `ThunkRef`s?
- `force()` is called on `k` before matching in `apply_cont`. Is this sufficient?
- What about the fields of `Val` and `E`? Are they also potentially thunked?
- Does `force` need to be called on `fields[0]` and `fields[1]` before using them?

### Q7: The `Val(x) >>= f = f x` Optimization

In freer-simple, `Val x >>= f` short-circuits to `f x` without going through the effect machine. After GHC optimization:
- Does this case get compiled away entirely (no `Val` constructor in optimized code)?
- Or does the `>>=` pattern match survive and we might see `Val` in the middle of a computation?
- If the evaluator encounters `Val(x)` when it expected `E(...)`, does the effect machine handle this correctly?

### Q8: Multiple Handler Layers

freer-simple programs are typically run with nested handlers:
```haskell
run . runConsole . runRng $ game
```

But our Rust effect machine handles ALL effects in a single flat dispatch loop. This is a fundamentally different architecture.

- In freer-simple, each `run*` peels off one effect from the type-level list, decrementing tags for remaining effects.
- Our flat dispatch does `tag - 1` in the HList traversal, which should be equivalent.
- **But**: Does the Haskell compilation produce a single `Eff '[Console, Rng]` program, or does it produce nested `run` calls that our evaluator needs to evaluate?
- Since we're serializing the RAW `game` binding (before any `run*` is applied), we should see `E(Union(tag, req), k)` directly. The Rust side IS the `run*` implementation. Verify this is correct.

## Concrete Test Cases

Please provide the expected behavior for each:

1. **Pure return**: `Val(42)` → should return `42`
2. **Single effect**: `E(Union(0, Emit("hi")), Leaf(\() -> Val(())))` → emit "hi", return ()
3. **Chained effects**: `emit "a" >> emit "b" >> return ()` — what does the tree look like and how does the machine step through it?
4. **Effect with continuation**: `awaitInt >>= \n -> emit (show n) >> return ()` — how does the machine thread the continuation after getting the int?
5. **Recursive effect**: `guessLoop target` where the loop body contains effects and a recursive call — does the machine handle the recursion correctly, or does it need tail-call optimization?

## Expected Output

1. **Correctness verdict**: For each question, is the Rust implementation correct, incorrect, or correct-but-fragile?
2. **If incorrect**: Exact fix with code
3. **If fragile**: What assumptions could break and how to make it robust
4. **Worked example**: Step-by-step trace of the effect machine executing `emit "hello" >> awaitInt >>= \n -> emit (show n)` showing every `apply_cont` call, closure application, and state transition
