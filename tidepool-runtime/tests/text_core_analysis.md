# Analysis of GHC Core for `T.length`

## Observations from GHC Core (`text-2.1.2`)

The `textLength` function compiles to the following optimized Core:

```haskell
textLength :: Text -> Int
  = \ (x_a9ba [Dmd=1!P(L,L,1L)] :: Text) ->
      case x_a9ba of { Text bx_a9eA bx1_a9eB bx2_a9eC [Dmd=1L] ->
      join {
        $j2_a9ey [Dmd=1C(1,!P(L))] :: Int# -> Int
        [LclId[JoinId(1)(Nothing)],
         Arity=1,
         Str=<L>,
         Unf=Unf{Src=<vanilla>, TopLvl=False,
                 Value=True, ConLike=True, WorkFree=True, Expandable=True,
                 Guidance=IF_ARGS [0] 11 10}]
        $j2_a9ey (x1_a9ez [OS=OneShot] :: Int#)
          = I# (negateInt# x1_a9ez) } in
      case bx2_a9eC of wild1_a9eE {
        __DEFAULT ->
          case {__ffi_static_ccall_unsafe text-2.1.2-9a59:_hs_text_measure_off :: ByteArray#
                                                                   -> Word64#
                                                                   -> Word64#
                                                                   -> Word64#
                                                                   -> State# RealWorld
                                                                   -> (# State# RealWorld,
                                                                         Int64# #)}_a9eF
                 bx_a9eA
                 (int64ToWord64# (intToInt64# bx1_a9eB))
                 (int64ToWord64# (intToInt64# wild1_a9eE))
                 9223372036854775807#Word64
                 realWorld#
          of
          { (# _ [Occ=Dead, Dmd=A], ds11_a9eI #) ->
          jump $j2_a9ey (int64ToInt# ds11_a9eI)
          };
        0# -> jump $j2_a9ey 0#
      }
      }
```

### Key Findings:
1.  **FFI Signature**: `_hs_text_measure_off` takes 4 arguments (plus `State#`): `arr`, `off`, `len`, `n`.
2.  **`T.length` Logic**: It calls `_hs_text_measure_off` with `n = 9223372036854775807` (which is `maxBound :: Int64`) and THEN negates the result.
3.  **Expected Return**: For `T.length` to return a positive result (like 5 for "hello"), the FFI call MUST return a negative value (like -5).

## The Bug in Tidepool

### 1. Codegen Bug (`primop.rs`)
The `PrimOpKind::FfiTextMeasureOff` handler only passes the first 3 arguments to the runtime function, discarding the 4th argument (`n`).

### 2. Runtime Bug (`host_fns.rs`)
The `runtime_text_measure_off` implementation is incorrect:
-   It only takes 3 arguments.
-   It treats the 3rd argument (`len`, which is the number of BYTES in the slice) as the number of characters to count.
-   It returns the number of BYTES.
-   It does not support returning a negated character count when requested.

### Path to Failure:
1.  Haskell calls `T.length "hello"`.
2.  Core calls `_hs_text_measure_off(arr, 0, 5, 9223372036854775807)`.
3.  Rust `emit_primop` calls `runtime_text_measure_off(addr, 0, 5)`, discarding `9223372036854775807`.
4.  Rust `runtime_text_measure_off` sees `len=5`, so it counts 5 characters, finds 5 bytes, and returns `5`.
5.  Haskell Core negates the result: `negateInt# 5 = -5`.
6.  Result is `-5`.

## Hypothesis
If we fix `runtime_text_measure_off` to take 4 arguments and return a negated character count when `n` is `maxBound` (or negative), the negation in Haskell Core will yield the correct positive length.
