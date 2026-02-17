# Locked Design Decisions

These decisions are frozen. Specs reference them by number. Do not revisit without human approval.

---

## D1: Join Point Encoding — Option C

`Join` + `Jump` as distinct CoreFrame variants, plus `LetNonRec`/`LetRec` split. Matches GHC Core 1:1.

CoreFrame binding variants: `LetNonRec`, `LetRec`, `Join`.
CoreFrame control variants: `App`, `Jump`.

Most faithful representation. Avoids subtle bugs from merging binding forms.

## D2: Type Information — Option B (Strip All)

Strip all types during serialization. CoreFrame carries no type annotations. DataCon metadata table (tag, representation arity, strictness) is sufficient for evaluation and `FromCore`/`ToCore` bridging. TypeFrame is vestigial or removed entirely.

Erasure happens in the Haskell serializer, before CBOR encoding:
- `Cast e _` → translate `e` (newtype coercion erasure)
- `Tick _ e` → translate `e` (profiling annotation erasure)
- `App e (Type _)` → translate `e` (type application erasure)
- `Type`, `Coercion` constructors → omitted entirely

## D3: Recursive Structure — Option A (Uniform)

`RecursiveTree<CoreFrame>` for everything. Single flat-vec representation. All recursion schemes (cata, ana, hylo) work uniformly over the same structure. Since types are stripped (D2), there's no TypeFrame to compose with.

## D4: GHC.Prim Boundary — Option C (Hybrid)

Let `-O2` inline standard library functions via normal cross-module inlining. Halt at `GHC.Prim` primitives (`+#`, `==#`, `indexArray#`, etc.) and serialize them as a `PrimOp` variant in CoreFrame. Rust evaluator implements ~30-40 hardware-level operations natively. GHC's optimizer handles the high-level inlining; Rust handles the hardware-level operations.

## D5: Thunk Payload Sizing — Variable-Size

Size field in HeapObject header. GC reads size per object during copy. Allocator uses bump pointer with variable stride. No wasted memory on small objects. Slightly more complex than fixed-size but avoids 64-byte-per-object waste that compounds across millions of thunks in a lazy evaluator.

## D6: Closure Code Pointer — In Header, Null for Interpreter

`code_ptr: *const u8` field in closure header. Null when closure is interpreter-only (interpreter ignores the field, walks AST). Cranelift codegen fills it with the JIT function pointer. No side-table indirection, no sentinel values.

## D7: Indirection Representation — Pointer Indirection

`Evaluated` variant holds a `*mut HeapObject` pointer to the WHNF value elsewhere on the heap. Simple, uniform thunk size, extra dereference on access is acceptable for v1. GC follows the indirection pointer during copy, which naturally shortens chains.

---

## CoreFrame Variants (Exact)

From D1 + D4:

```rust
pub enum CoreFrame<A> {
    Var(VarId),
    Lit(Literal),
    App { fun: A, arg: A },
    Lam { binder: VarId, body: A },
    LetNonRec { binder: VarId, rhs: A, body: A },
    LetRec { bindings: Vec<(VarId, A)>, body: A },
    Case { scrutinee: A, binder: VarId, alts: Vec<Alt<A>> },
    Con { tag: DataConId, fields: Vec<A> },
    Join { label: JoinId, params: Vec<VarId>, rhs: A, body: A },
    Jump { label: JoinId, args: Vec<A> },
    PrimOp { op: PrimOpKind, args: Vec<A> },
}

pub enum AltCon {
    DataAlt(DataConId),
    LitAlt(Literal),
    Default,
}

pub struct Alt<A> {
    pub con: AltCon,
    pub binders: Vec<VarId>,  // bound pattern variables (empty for LitAlt/Default)
    pub body: A,
}
```

**Case binder:** GHC Core's `case e of x { alts }` binds `x` to the evaluated scrutinee. Alternatives may reference the case binder when they need the whole value (not just destructured fields). The case binder is always present — if unused, the evaluator can ignore it, but it must exist in the representation.

**`Con` and `PrimOp` are serialization sugar.** GHC Core has no `Con` expression — data constructors are `Id`s applied via nested `App`. The Haskell serializer recognizes *saturated* constructor applications and collapses them into `Con { tag, fields }`. Unsaturated constructor applications (a constructor passed as a function, e.g. `map Just xs`) serialize as `Var` — the Rust side treats them as closures. Same for `PrimOp`: saturated primop applications collapse into `PrimOp { op, args }`. Unsaturated primops should not appear after `-O2` (GHC saturates them); if encountered, the serializer should error.

## HeapObject Layout (Exact)

From D5 + D6 + D7:

**This is a memory layout specification, not a Rust enum.** Variable-length payloads (`captured`, `fields`) cannot be expressed as Rust enum variants. Implement as raw byte buffers with unsafe accessor methods. The pseudo-enum below defines the logical variants and fields:

```
Logical layout (not valid Rust):

HeapObject {
    tag: u8,          // offset 0 — variant discriminant
    size: u16,        // offset 1 — total object size in bytes (including header)
    // variant-specific payload follows at offset 3:
    Closure { code_ptr: *const u8, num_captured: u16, captured: [*mut HeapObject; num_captured] },
    Thunk(ThunkState), // Unevaluated { env_ptr, expr_ptr } | BlackHole | Evaluated(*mut HeapObject)
    Con { con_tag: DataConId, num_fields: u16, fields: [*mut HeapObject; num_fields] },
    Lit(Literal),
}
```

Tag byte at offset 0. Cranelift `br_table` and Rust `match` dispatch on the same byte. Variable-size — GC, allocator, and codegen all read size from offset 1. All objects aligned to 8 bytes (allocator rounds up). Exact byte offsets for each variant's fields must be defined in the scaffold and shared across core-heap and codegen.

## Open: Unboxed Types

**Status: needs design decision.** GHC's `-O2` aggressively unboxes via worker/wrapper. A function `Int -> Int -> Int` becomes a worker taking `Int# -> Int# -> Int#` arguments. After the simplifier, most numeric code operates on unboxed `Int#`, `Double#`, `Word#`, `Char#`:

```haskell
case x of { I# n -> case y of { I# m -> case +# n m of r -> I# r } }
```

The `PrimOp` variant already handles `+#` etc., so unboxed *operations* are covered. The question is whether `Literal` and `Value` need explicit unboxed variants, or whether the existing representation (Lit for all literals, PrimOp args/results are just Values) is sufficient. In the interpreter, this may "just work" — `+#` takes two `Value::Lit(LitInt(n))` and returns `Value::Lit(LitInt(result))`. The boxed/unboxed distinction only matters for heap layout (unboxed values live in registers, never heap-allocated).

For v1, the likely answer is: `Literal` is always a raw value (i64, f64, etc.), boxing is explicit via `Con(I#, [Lit(42)])`, and the evaluator doesn't distinguish boxed from unboxed at the Value level. But this should be confirmed against actual `-O2` Core output from freer-simple programs.

---

## GHC Pipeline

GHC 9.12.3. freer-simple assumed compatible (no plugin, simple package).

Pipeline: `parseModule` → `typecheckModule` → `hscDesugar` → `core2core`.
Captures `ModGuts` after `core2core` (post-simplifier, pre-tidy).
DynFlags: `backend=noBackend`, `ghcLink=NoLink`, `updOptLevel 2`.
Package DB: inherits `GHC_PACKAGE_PATH` from nix environment.

Core output under `-O2`: `Val`/`E` constructors with `Union` members. No GHC plugin required.

## freer-simple Architecture (empirically verified — see research/01)

`Eff` is `Free (Union r) a`. Under `-O2`, interpreted effects collapse (catamorphisms over the free monad that fuse away), uninterpreted effects survive as `E` constructors carrying continuations — the yield points.

### Internal Constructors (from -O2 Core dump)

- **`Val x`** — pure result (`Pure` in the Haskell API)
- **`E (Union tag# request) k`** — effect request with continuation
- **`Leaf f`** — single continuation step (`>>= f`)
- **`Node k1 k2`** — composed continuation (binary tree)

The continuation `k` is a **type-aligned sequence** (binary tree of `Leaf`/`Node`), NOT a single closure. `Leaf` wraps one `a -> Eff r b` closure. `Node` composes two continuations. Applying a continuation: case-split on `Leaf f` → call `f arg`; `Node k1 k2` → apply `k1` to arg, compose result with `k2`.

### Union Encoding

Effects in the type-level list are indexed by **unboxed `Word#` tags** (compile-time constants):
- `Union 0##` = first effect in `'[E1, E2, ...]`
- `Union 1##` = second effect
- etc.

The Rust-side dispatcher extracts the tag as a machine integer — no heap allocation for the tag itself.

### EffectMachine

A ~30-line Rust `Iterator` wrapper: `step()` evaluates to WHNF, destructures `E (Union tag req) k`, returns `Yield::Request(tag, req)` or `Yield::Done(val)`. `resume(result)` applies the continuation tree to `result` and loops.

Effect finalization via `run` (freer-simple's pure runner). No IO contamination.
