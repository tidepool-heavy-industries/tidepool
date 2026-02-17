# Research: freer-simple -O2 Core Output

**Priority:** CRITICAL — validates the entire tidepool premise
**Status:** COMPLETE — compiled with GHC 9.12.2 -O2, Core dump analyzed
**POC code:** `/tmp/freer-core-dump/`
**Core dump:** `/tmp/freer-core.simpl`

## Summary

The tidepool premise is **validated**. freer-simple -O2 Core has the expected structure:
- `E (Union tag# request) continuation` = yield point (uninterpreted effect)
- `Val result` = pure result
- `evalState3` handles State effects at compile time when the effect is fully interpreted

## Detailed Findings

### 1. Core Structure — CONFIRMED

freer-simple's internal constructors:
- **`Val x`** = `Pure x` — pure result value
- **`E union_req k`** = `Free (Send req cont)` — an effect request with continuation
- **`Leaf f`** = single continuation function `f :: a -> Eff r b`
- **`Node k1 k2`** = composed continuation (`>>=` chain, implemented as a type-aligned sequence)

A fully uninterpreted program like `multiEffect` compiles to a chain:
```
multiEffect = E (Union 0## GetThing) (Node ... (Leaf (\content -> E (Union 1## (WriteFile ...)) ...)))
```

This is exactly the shape the EffectMachine expects: evaluate to WHNF, get `E union k`, dispatch on the Union tag, resume with the continuation.

### 2. Union Encoding — CONFIRMED

Effects in the type-level list are indexed by **unboxed `Word#` tags**:
- `Union 0##` = first effect in the list
- `Union 1##` = second effect in the list

Example from `multiEffect :: Eff '[MyEffect, FileSystem] ()`:
- `MyEffect` operations → `Union 0##`
- `FileSystem` operations → `Union 1##`

The tags are **compile-time constants** (unboxed word literals). The runtime never computes them — they're baked into the code.

### 3. Unboxed Types — PRESENT IN -O2 CORE

Unboxed arithmetic appears where State effects are interpreted:

```haskell
-- From myProgram (State Int effect, partially run):
case n of { I# x -> Put (I# (+# x 1#)) }
```

The pattern `case n of { I# x -> ... (+# x 1#) ... I# ... }` shows:
- Unboxing via `I# x` pattern match
- Unboxed primop `+#`
- Re-boxing via `I#` constructor

This is standard GHC worker/wrapper transformation. The **boxed** `Int` enters, gets unboxed to `Int#`, arithmetic happens on `Int#`, result gets re-boxed.

**Key for tidepool:** The unboxed values `x` and `1#` are transient — they exist between the unbox (`I#` pattern) and rebox (`I#` application). The evaluator/codegen needs to handle `Int#` values, but they never escape into the heap. They flow through registers, not heap objects.

### 4. PrimOps Found

| PrimOp | Context |
|--------|---------|
| `+#` | Integer addition (State increment) |
| `unpackCString#` | String literal unpacking |

Minimal — this is a small program. A real program would add comparison (`>#`, `==#`), array ops, etc. But the pattern is clear: primops appear **saturated** (always fully applied) and operate on **unboxed** values.

### 5. Data Constructors — ALL SATURATED

Every data constructor application in the Core dump is fully saturated:
- `I# 0#`, `I# 42#`, `I# (+# x 1#)` — boxed Int
- `Val x` — freer-simple pure result
- `E union k` — freer-simple effect request
- `Union 0## req` — Union tagging
- `Leaf f`, `Node k1 k2` — continuation building
- `Put x`, `GetThing`, `PutThing s`, `ReadFile p`, `WriteFile p s` — effect constructors

No unsaturated constructor applications were found. This confirms that the serializer can treat Con as syntactic sugar for saturated application.

### 6. Join Points

Header: `{terms: 301, types: 1,316, coercions: 22, joins: 0/6}`

6 non-recursive join points exist but are suppressed in this dump mode. They're used for case continuations and let-binding chains. The evaluator needs to handle them (they're essentially labeled continuations within a function body).

### 7. State Handler Fusion

`partiallyRun = evalState3 main8 myProgram` — the State handler is **NOT fully inlined** at the top level. It's a function call to `evalState3`. However, where it's fully applied (in `main4`), GHC does pattern-match on the result:

```haskell
case evalState3 main8 main6 of {
    Val x -> ...     -- Pure result path
    E _ _ _ -> case run1 of {}  -- Impossible path (all effects handled)
}
```

The `E` branch is dead code (`case run1 of {}` = bottom). GHC knows that after running all handlers, only `Val` is possible, but it keeps the dead branch as a type-safety guard.

### 8. Continuation Shape

The continuation `k` in `E req k` is built from `Leaf` and `Node`:
- **`Leaf f`** = a single `>>= f` step
- **`Node k1 k2`** = composition of two continuations

At runtime, applying a continuation means: case-split on `Leaf f` → call `f arg`, or `Node k1 k2` → apply `k1` to arg, then compose with `k2`.

The continuation is a **type-aligned sequence** (efficient append via binary tree structure), not a simple function closure. This matters for the Rust evaluator: the continuation is a heap-allocated tree of closures, not a single closure.

## Implications for Tidepool

1. **EffectMachine step/resume is correct.** `E (Union tag req) k` is exactly the yield point structure. Step returns the tag + request, resume applies the continuation.

2. **Union tag is an unboxed Word#.** The Rust-side dispatcher can extract it directly as a machine integer. No heap allocation for the tag.

3. **Continuations are trees (Leaf/Node), not closures.** The evaluator must handle both. This affects HeapObject layout — need variants for Leaf (wraps a closure) and Node (two child continuations).

4. **Unboxed types exist but are transient.** `Int#` etc. appear between unbox/rebox but don't escape to the heap. The evaluator needs a register or stack representation for unboxed values, but not heap objects.

5. **All constructor applications are saturated.** The plan's assumption that Con is syntactic sugar for saturated application is correct.

6. **freer-simple builds on GHC 9.12 with `allow-newer`.** The TH module compiles fine — no patching needed.
