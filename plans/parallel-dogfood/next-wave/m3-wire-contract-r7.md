# M3 prepared-execution wire contract r7

Status: released shared seam for the M3 writer and decoder children. This is an
internal format with no compatibility window. M6 changes every production
writer/reader together, rejects stale artifacts and deletes the old path.

The encoding is definite-length CBOR arrays and byte/text/integer leaves. Maps,
indefinite lengths, unknown tags, wrong field counts and trailing bytes are
rejected. Unsigned integers must fit the destination width. Dense IDs are CBOR
unsigned integers, zero based, and are interpreted only in the field's declared
ID namespace. Text is UTF-8; source string/address literals are bytes.

The root is:

```text
["TPSTG", schemaVersion, projectionProfile, toolchain,
 executionAbiVersion, target, signatures, globals, constructors, operations,
 bindingGroups, entryValueId]
```

`target = [architecture, endianness, pointerWidth, wordWidth, abi, features]`,
where architecture is `0=x86_64 | 1=aarch64`, endianness is
`0=little | 1=big`, widths are bits, and features is a sorted unique text array.
`symbol = [unit,module,namespace,occurrence]`. Exact symbol equality uses all
four fields.

Closed sums use an array beginning with the numeric tag:

```text
rep       [0] Void | [1] LiftedRef | [2] UnliftedRef | [3] Address
          | [4,bits] Int | [5,bits] Word | [6,bits] Float
valueRef  [0,valueId] Local | [1,globalId] Global
scalar    [0,bits,bytes] Int | [1,bits,bytes] Word
          | [2,bits,bytes] Float | [3,codepoint] Char | [4,bytes] Bytes
atom      [0,valueRef] Ref | [1,scalar] Scalar | [2] Void
group     [0,item] NonRecursive | [1,items] Recursive
update    0 Memoize | 1 SingleEntry
pattern   [0] Default | [1,constructorId] Constructor | [2,scalar] Literal
```

Integer and word scalar bytes are exactly `ceil(bits/8)` bytes, big endian;
signed integers use two's complement. Float bytes preserve IEEE payload bits.
Char is a checked Haskell codepoint (`0..0x10ffff`, including surrogate code
points) and is not converted through Rust `char`.

Records and expression sums are:

```text
signature       [argumentReps,resultReps]
fieldLayout     [rep,offset]
checkedLayout   [fieldLayouts,alignment,payloadSize,rootMask]
constructorDecl [symbol,familySymbol,fieldReps,strictFields,checkedLayout]
globalDecl      [symbol,signatureId,requiredEvaluated,requiredGeneration]
operationDecl   [identityText,signatureId]
heapBinding     [valueId,heapRhs]
heapRhs         [0,signatureId,parameters,captures,body] Function
                [1,signatureId,update,captures,body] Thunk
                [2,constructorId,fields] Constructor
joinBinding     [joinId,signatureId,parameters,body]
alternative     [pattern,binders,body]
topBinding      [symbol,heapBinding]

expr [0,atoms] Return
     [1,atom,signatureId] Enter
     [2,callee,signatureId,arguments] Call
     [3,operationId,arguments] Operation
     [4,constructorId,fields] Construct
     [5,scrutinee,binder,resultReps,alternatives] Case
     [6,bindingGroup,body] Let
     [7,joinGroup,body] LetJoins
     [8,joinId,arguments] Jump
```

`requiredGeneration` is `[0]` for a static source import or `[1,generation]`
for a retained resident value. The producer records the exact resident binding
generation; the linker rejects a same-name value from any other generation.

The producer emits deterministic table and dependency-group order. The decoder
enforces configured byte/node/table/string/depth/work limits before publishing
`PreparedProgram`; validates versions/requirements, dense references, duplicate
symbols/IDs, lexical scope, recursive-group visibility, signatures, alternatives,
layouts and entry; and rejects unreachable expression storage if it introduces
another representation. The linker resolves every global against one immutable
`MachineImports` snapshot and publishes `LinkedProgram` only after all identity,
signature, evaluatedness and generation checks succeed.

The first valid semantic fixture is
`haskell/test-prepared-stg/M3Vertical.hs`; its prepared output must retain the
import, recursion and strict field it claims. Decoder tests mutate its encoded
artifact for stale version, target mismatch, truncation/trailing input, wrong ID
kind/range, scope, duplicate definition, signature/layout and import failures.
