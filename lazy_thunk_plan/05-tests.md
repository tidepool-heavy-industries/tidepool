# WS3: Test Plan

## Test Levels

### Level 1: Unit tests for heap_force (Rust-only, no Haskell)

File: `tidepool-codegen/src/host_fns.rs` (add tests module)

Manually construct thunk heap objects in a byte buffer and test `heap_force`:

1. **Force unevaluated thunk** — state transitions to Evaluated, indirection
   written, correct result returned
2. **Force evaluated thunk** — returns cached indirection immediately
3. **Force BlackHole** — triggers trap/error
4. **Double force** — first force evaluates, second returns cached
5. **Null thunk pointer** — returns null (existing behavior)

These tests need a simple test entry function (extern "C" fn that returns
a Lit heap object). Can use the existing test infrastructure in host_fns.rs.

### Level 2: Haskell fixture tests (Core → CBOR → JIT)

File: `haskell/test/Suite.hs` — add new test bindings
File: `tidepool-eval/tests/haskell_suite.rs` — add test cases

New Haskell test bindings:

```haskell
-- Infinite list producers (currently crash)
thunk_repeat :: [Int]
thunk_repeat = take 5 (repeat 1)
-- Expected: [1, 1, 1, 1, 1]

thunk_iterate :: [Int]
thunk_iterate = take 5 (iterate (+1) 0)
-- Expected: [0, 1, 2, 3, 4]

thunk_cycle :: [Int]
thunk_cycle = take 7 (cycle [1, 2, 3])
-- Expected: [1, 2, 3, 1, 2, 3, 1]

-- Multi-input consumer (the motivating case)
thunk_zipwith :: [Int]
thunk_zipwith = zipWith (+) [10, 20, 30] [0..]
-- Expected: [10, 21, 32]

-- zipWith with two infinite lists + take
thunk_zipwith_inf :: [Int]
thunk_zipwith_inf = take 4 (zipWith (+) [0..] [100..])
-- Expected: [100, 102, 104, 106]

-- Nested thunks
thunk_map_inf :: [Int]
thunk_map_inf = take 5 (map (*2) [0..])
-- Expected: [0, 2, 4, 6, 8]

-- BlackHole detection
thunk_blackhole :: Int
thunk_blackhole = let x = x in x
-- Expected: clean error (not SIGSEGV)

-- Existing LetRec knot-tying still works
thunk_letrec_knot :: [Int]
thunk_letrec_knot = let xs = 1 : xs in take 5 xs
-- Expected: [1, 1, 1, 1, 1]
```

### Level 3: MCP eval smoke tests

After rebuilding the MCP server binary, verify interactively:

```haskell
pure (take 5 (repeat 1 :: [Int]))
-- → [1,1,1,1,1]

pure (zipWith (+) [10,20,30] ([0..] :: [Int]))
-- → [10,21,32]

pure (take 3 (iterate (+1) (0 :: Int)))
-- → [0,1,2]

pure (take 7 (cycle [1,2,3 :: Int]))
-- → [1,2,3,1,2,3,1]

-- BlackHole
let x = x :: Int in pure x
-- → error: infinite loop detected (or similar)
```

### Level 4: Regression tests

Run the full existing test suite to ensure no regressions:

```bash
cargo test --workspace
```

Key concern: thunkifying Con fields changes evaluation order. Programs that
accidentally relied on eager field evaluation might behave differently.
The existing 100+ test bindings in `haskell/test/Suite.hs` are the primary
regression safety net.

## Test Fixture Compilation

The Haskell test fixtures need to be compiled to CBOR and checked in:

```bash
cd haskell
cabal run tidepool-extract -- test/Suite.hs --all-closed
# This regenerates test/suite_cbor/*.cbor
```

The `thunk_blackhole` test is special — it may need to be tested differently
since it's expected to error, not produce a value.

## Priority Order

1. Unit tests for heap_force (can start immediately, no Haskell needed)
2. Haskell fixtures (can start immediately, independent of Rust changes)
3. Integration tests (requires WS1 + WS2 complete)
4. MCP smoke tests (requires full rebuild)
5. Regression suite (final gate)
