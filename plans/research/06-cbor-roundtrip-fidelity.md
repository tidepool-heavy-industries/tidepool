# Deep Research: CBOR Serialization Round-Trip Fidelity

## Problem Statement

Tidepool serializes GHC Core ASTs to CBOR on the Haskell side, then deserializes on the Rust side. The serialization must be perfectly faithful — any mismatch in variable identity, constructor tags, literal encoding, or tree structure will cause silent runtime bugs.

We have a suspected bug where the deserialized tree evaluates to `I# 1` instead of `E(Union, FTCQueue)`. Even if this turns out to be a `findTargetId` bug (see research/04), we need to verify the CBOR round-trip is correct to have confidence in the rest of the pipeline.

## Serialization Pipeline

### Haskell Side (Encode)

**Translate.hs** converts `CoreExpr` → `Seq FlatNode` where:
```haskell
data FlatNode
  = NVar !Word64              -- Variable reference (Unique key)
  | NLit !LitEnc              -- Literal value
  | NApp !Int !Int            -- Application (fun_idx, arg_idx)
  | NLam !Word64 !Int         -- Lambda (binder_id, body_idx)
  | NLetNonRec !Word64 !Int !Int  -- Let (binder_id, rhs_idx, body_idx)
  | NLetRec ![(Word64, Int)] !Int -- LetRec ([(binder_id, rhs_idx)], body_idx)
  | NCase !Int !Word64 ![FlatAlt] -- Case (scrut_idx, binder_id, alts)
  | NCon !Word64 ![Int]       -- Constructor (datacon_id, field_idxs)
  | NJoin !Word64 ![Word64] !Int !Int  -- Join point
  | NJump !Word64 ![Int]      -- Jump to join point
  | NPrimOp !Text ![Int]      -- Primitive operation
```

**CborEncode.hs** serializes `Seq FlatNode` → CBOR bytes via `Codec.Serialise`.

### Rust Side (Decode)

**core-repr/src/serial/read.rs** deserializes CBOR → `RecursiveTree<CoreFrame<usize>>` where:
```rust
pub enum CoreFrame<A> {
    Var(VarId),
    Lit(Literal),
    App(A, A),
    Lam(VarId, A),
    LetNonRec { binder: VarId, rhs: A, body: A },
    LetRec { bindings: Vec<(VarId, A)>, body: A },
    Case { scrut: A, binder: VarId, alts: Vec<Alt<A>> },
    Con { tag: DataConId, fields: Vec<A> },
    PrimOp { op: PrimOpName, args: Vec<A> },
}
```

## Research Questions

### Q1: VarId Encoding/Decoding

Variables are identified by `Word64` derived from `getKey (varUnique v)` in Haskell.

- **Haskell side**: `varId v = fromIntegral (getKey (varUnique v))` — `getKey` returns `Int`, which is cast to `Word64`. Is this safe? GHC Uniques can be negative?
- **Rust side**: `VarId` wraps a `u64`. How is it decoded from CBOR? Is the CBOR encoding signed or unsigned integer?
- **Potential bug**: If `getKey` returns a negative `Int` and `fromIntegral` wraps it to a large `Word64`, does the Rust side decode the same value? CBOR distinguishes signed (major type 1) from unsigned (major type 0) integers.
- **DataConId encoding**: Constructor IDs also use `getKey (varUnique (dataConWorkId dc))`. Same `Int → Word64` conversion. Same potential sign issue.

### Q2: Constructor Tag vs DataConId

The Haskell side encodes constructors as `NCon !Word64 ![Int]` where the Word64 is `varId (dataConWorkId dc)` — the Unique of the worker Id.

The Rust side has:
```rust
Con { tag: DataConId, fields: Vec<A> }
```

- Is `DataConId` the same as the Unique-based ID? Or is it supposed to be the constructor tag (1-indexed position in the data type)?
- The `DataConTable` maps `DataConId → (name, tag, arity, bangs)`. Is the key in the table the Unique-based ID or the positional tag?
- If the Haskell side sends Unique-based IDs but the Rust side expects positional tags, every constructor match will fail.

### Q3: Flat Node Indexing

Nodes reference each other by index into the `Seq FlatNode`. The root is the last node (index = len - 1).

- **CBOR format**: Is the array 0-indexed? Does the Rust reader use the same indexing convention?
- **Root convention**: Haskell writes `[nodes_array, root_idx]` in CBOR. Does `root_idx` always equal `len(nodes_array) - 1`? Or can it differ?
- **Index offsets**: If there's an off-by-one between Haskell's `Seq.length` and Rust's `Vec::len()`, every cross-reference will be wrong.

### Q4: Literal Encoding

```haskell
data LitEnc
  = LEInt !Int64
  | LEWord !Word64
  | LEChar !Word32
  | LEString !ByteString
  | LEFloat !Word64
  | LEDouble !Word64
```

- **String encoding**: Is `LEString` encoded as CBOR bytes (major type 2) or CBOR text (major type 3)? Haskell's `ByteString` would be bytes, but GHC's `LitString` is a `ByteString` representing... what exactly? UTF-8 text? Raw bytes?
- **Int64 sign handling**: CBOR uses different major types for positive (0) and negative (1) integers. Does the Rust decoder handle both correctly for `LEInt`?
- **Word64 encoding**: `LEWord` is unsigned. CBOR major type 0. Does the Haskell encoder use `encodeWord64` or `encodeInteger`?
- **IEEE 754 doubles**: `LEDouble` stores raw bits as `Word64`. Is this encoded as a CBOR integer (the raw bits) or a CBOR float? The Rust side needs to decode the same way.

### Q5: Case Alternatives Encoding

```haskell
data FlatAlt = FlatAlt !FlatAltCon ![Word64] !Int
data FlatAltCon = FDataAlt !Word64 | FLitAlt !LitEnc | FDefault
```

- How are alternatives ordered? GHC requires `DEFAULT` first if present. Does the CBOR encoding preserve this order?
- **DataAlt encoding**: `FDataAlt !Word64` — this is the DataCon worker Unique. Same question as Q2: does Rust expect this or a positional tag?
- **Missing alternatives**: If GHC's case-of-known-constructor eliminated some alternatives, are the remaining ones still correctly encoded?

### Q6: Metadata (DataConTable) Round-Trip

The metadata file `meta.cbor` contains:
```haskell
type MetaEntry = (Word64, Text, Int, Int, [Text])
-- (datacon_unique, name, tag, arity, bangs)
```

- How is this encoded? Array of arrays? Map?
- Does the Rust side reconstruct a `DataConTable` that maps `DataConId → MetaEntry`?
- If the key is the Unique-based Word64, and the `Con` nodes also use Unique-based IDs, then lookups should work. But verify this.
- Are ALL constructors that appear in the tree present in the metadata? Or could there be missing entries for built-in types (`I#`, `()`, `True`, `False`, etc.)?

### Q7: Join Points and Jumps

```haskell
NJoin !Word64 ![Word64] !Int !Int  -- (binder, params, rhs_idx, body_idx)
NJump !Word64 ![Int]               -- (label, arg_idxs)
```

- Does the Rust side have corresponding `CoreFrame` variants? Looking at the Rust enum, I see no `Join` or `Jump` variants. If GHC produces join points (common at -O2), and they're encoded in CBOR, but the Rust decoder doesn't handle them, deserialization will fail or produce garbage.
- What is the CBOR tag/discriminant for Join and Jump? Does the Rust decoder skip unknown tags or error?

### Q8: Diagnostic: Binary Comparison

To verify round-trip fidelity without trusting either side's pretty-printer:
- What tool can dump raw CBOR structure? (`cbor-diag`, `cborg` CLI, Python `cbor2`)
- How to generate a "reference" CBOR from known-good Haskell Core and compare byte-for-byte?
- What diagnostic prints should we add to the Rust deserializer to verify each node as it's read?

## Files to Examine

### Haskell
- `haskell/src/Tidepool/CborEncode.hs` — CBOR encoding logic
- `haskell/src/Tidepool/Translate.hs` — Core → FlatNode translation

### Rust
- `core-repr/src/serial/read.rs` — CBOR decoding
- `core-repr/src/serial/mod.rs` — Serial module structure
- `core-repr/src/lib.rs` — CoreFrame definition and RecursiveTree

## Environment

- **Haskell CBOR library**: `serialise` (Codec.Serialise) — uses CBOR major types directly
- **Rust CBOR library**: `ciborium` — serde-based CBOR
- **GHC**: 9.12.2
- **File**: `examples/guess/Guess.hs` produces the test CBOR at build time

## Expected Output

1. **For each question**: Identify whether there's a real mismatch, cite the relevant encode/decode code, and provide a fix if needed
2. **CBOR schema documentation**: What the binary format actually is, field by field
3. **Diagnostic script**: A way to dump a .cbor file and verify its structure matches expectations (e.g., Python script using `cbor2`, or Rust test)
4. **List of invariants** that must hold between encode and decode sides
