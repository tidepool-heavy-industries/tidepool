# Generic-codec spike — GO, with receipts

**Verdict: GO** (2026-08-08). PRD delivery step 1
([`14-generic-derived-askuser-prd.md`](14-generic-derived-askuser-prd.md))
is discharged: `deriving (Generic)` + a valueless `askUser @T`-shaped codec
elaborates and executes through the real extract/JIT, and selector-aware
`TypeError` diagnostics fire at the source level.

Everything below was run against the DEPLOYED extract + resident JIT through
`tidepool-repl`'s `session_run` — the production path, not a hand-wired
harness. Each block is a real compile through `tidepool-extract` and a real
execution on the JIT machine.

## What the spike proved

### 1. Generic instance elaboration — round-trips on the JIT

```haskell
data Color3 = Red3 | Green3 | Blue3 deriving (Show, Generic)
roundTrip3 :: Color3 -> Color3
roundTrip3 = G.to . G.from
```

`map roundTrip3 [Red3, Green3, Blue3]` → `[Red3,Green3,Blue3]`.

### 2. Type-level Symbol reflection — the unproven part, proven

The load-bearing risk was `Constructor`/`Selector`/`Datatype` metadata:
their instances reflect promoted `Symbol`s, which is a different elaboration
path from ordinary dictionary passing. A hand-written `GConName` traversal
over `D1`/`:+:`/`C1` returned `"Red4,Green4,Blue4"` — constructor names,
in declaration order, recovered from type metadata alone.

### 3. Valueless shape derivation over a nested sum/product

A full `FormShape` interpreter (`GForm`/`GCons`/`GFields` + a `FormValue`
leaf class with `DefaultSignatures`) applied to:

```haskell
data Environment = Development | Staging | Production deriving (Show, Generic)
data Destination
  = LocalHost
  | Ssh { host :: Text, port :: Int }
  | Container { image :: Text }
  deriving (Show, Generic)
data DeployRequest = DeployRequest
  { service       :: Text
  , environment   :: Environment
  , destination   :: Destination
  , replicas      :: Int
  , runMigrations :: Bool
  , releaseNote   :: Maybe Text
  } deriving (Show, Generic)

instance FormValue Environment    -- empty; DefaultSignatures does the work
instance FormValue Destination
instance FormValue DeployRequest
```

`valShape (Proxy :: Proxy DeployRequest)` evaluated to, verbatim:

```text
ProductShape "DeployRequest"
  [ ("service",StringShape)
  , ("environment",SumShape "Environment"
      [("Development",UnitShape),("Staging",UnitShape),("Production",UnitShape)])
  , ("destination",SumShape "Destination"
      [ ("LocalHost",UnitShape)
      , ("Ssh",ProductShape "Ssh" [("host",StringShape),("port",IntShape)])
      , ("Container",ProductShape "Container" [("image",StringShape)])])
  , ("replicas",IntShape)
  , ("runMigrations",BoolShape)
  , ("releaseNote",OptionalShape StringShape)
  ]
```

Confirms, on the real JIT: exact selector keys, exact constructor keys,
declaration order preserved (the balanced `:+:` tree did NOT leak into
ordering), product-of-sum and sum-of-product nesting, `Maybe` as a blessed
container rather than a `Nothing`/`Just` picker, and empty-instance
`DefaultSignatures` discharge from `Generic` alone.

### 4. Recursive decode — typed value out the other side

`gDecode`/`valDecode` over the same generic structure, four submissions:

| Submission | Result |
|---|---|
| nested `Ssh` payload, `Just` note, `Staging` | `Right (DeployRequest {service = "api", environment = Staging, destination = Ssh {host = "example.com", port = 22}, replicas = 3, runMigrations = False, releaseNote = Just "hotfix"})` |
| empty `Text`, `0`, `True`, `Nothing`, nullary `LocalHost` | `Right (DeployRequest {service = "", environment = Development, destination = LocalHost, replicas = 0, runMigrations = True, releaseNote = Nothing})` |
| missing product field | `Left "missing field environment"` |
| unknown constructor tag | `Left "no constructor Nope | ..."` |

The PRD's "remain distinguishable" cases (`False`, zero, empty `Text`,
nullary payloads) all survive the round trip, and malformed answers are
rejected as data — the continuation is never consumed.

### 5. Selector-aware `TypeError` — three fixtures, source-level

A `SelKey`/`FieldCheck` closed type-family pair, dispatching on the `Meta`
carried by `S1`:

```haskell
type family SelKey (s :: Meta) :: Symbol where
  SelKey ('MetaSel ('Just n) su ss ds) = n
  SelKey ('MetaSel 'Nothing  su ss ds) = "<positional>"

type family FieldCheck (n :: Symbol) (a :: Type) :: Constraint where
  FieldCheck n [Char]          = TypeError (...)
  FieldCheck n [a]             = TypeError (...)
  FieldCheck n (Maybe (Maybe a)) = TypeError (...)
  FieldCheck n a               = ()
```

Actual GHC output through the extractor (the whole error, nothing elided):

```text
`tags` is a list; lists need a repeated-field editor and are not supported in v1.
`name :: String` is not supported; use Text.
`note` has nested optionality. Use one Maybe layer or an explicit domain sum.
```

No `M1`, no `K1`, no class name, no instance dump — the field name and a
correction. The good case (`Text`/`Int`/`Maybe Text`) compiles and runs.

## Frozen: the answer encoding

The spike freezes the recursive answer algebra as structural, not flat. Both
directions recurse over the same generic structure, so they cannot disagree:

```haskell
data FormAnswer
  = StringAnswer Text
  | IntAnswer Int
  | NumberAnswer Double
  | BoolAnswer Bool
  | UnitAnswer
  | OptionalAnswer (Maybe FormAnswer)
  | ProductAnswer [(FieldKey, FormAnswer)]
  | SumAnswer ConstructorKey FormAnswer
```

Rules frozen with it:

- A single-constructor datatype's answer is the bare `ProductAnswer` — no
  redundant `SumAnswer` wrapper. Multi-constructor datatypes always carry
  `SumAnswer conKey payload`.
- A nullary constructor's payload is `UnitAnswer`, never an empty product —
  so `LocalHost` and a hypothetical zero-field record stay distinguishable.
- Product fields are keyed by exact selector name; positional fields by
  one-based `"1"`, `"2"`, … scoped to their own product node (never a
  form-wide counter — PRD migration rule 5).
- `Maybe` is `OptionalAnswer`, never a constructor pick.
- The Rust wire form serializes this JSON-shaped at the transport boundary
  only. It is not an aeson contract; nothing routes through `FromJSON`.

## Findings the implementation must carry

1. **`GHC.Generics` must be imported qualified in any module that also has
   the Tidepool prelude in scope.** Bare `from`/`to` is an ambiguous
   occurrence against `Control.Lens.Iso.from` / `Control.Lens.Getter.to`,
   both re-exported by `Tidepool.Prelude`:

   ```text
   Ambiguous occurrence `from'. It could refer to either `Tidepool.Prelude.from',
   ... or `GHC.Generics.from'
   ```

   This is a live input to the `Tidepool.Harness.Prelude` decision: the
   curated re-export module must not put `from`/`to` in an author's
   unqualified scope alongside `Generic`. Authors never name `from`/`to`
   themselves — the codec does — so the fix belongs inside the codec module,
   but the collision is proof the prelude surface needs the same audit.

2. **Recursive types are a runtime divergence, not a compile error.**
   `data Tree = Leaf | Node { left :: Tree, right :: Tree }` with
   `instance FormValue Tree` COMPILES — the instance is found, and the
   recursion is at the value level. Shape production diverges only when
   forced. The PRD's visited-type set is therefore load-bearing and must be
   TYPE-level; do not expect the dictionary layer to catch it, and do not
   ship recursion rejection as "GHC will complain".

3. ~~**`conName`/`selName`/`datatypeName` via `(undefined :: C1 c f p)` is
   safe here**~~ — **CORRECTED during implementation.** The claim was true
   but not sufficient: the argument is never forced under the JIT, which is
   what the spike measured, but the tree-walking eval interpreter — the
   JIT's differential ORACLE — does force it. A bottom there diverges the
   two engines, which is exactly what the differential suites exist to
   catch. Vendored Aeson already carries this scar in a comment. Use real
   `Proxy` constructors for metadata, not `undefined`. The spike tested one
   engine and generalized to both; measuring the JIT alone does not settle
   a question about the oracle.

4. Left-to-right `:+:` decode with an accumulated error works but produces a
   repeated message (`"no constructor Nope | no constructor Nope | ..."`).
   The production interpreter should report the unknown tag once against the
   sum's own variant list rather than concatenating per-branch failures.

## Corrections found during implementation

Two specifics below did not survive implementation and are corrected in
place above.

- **The recursion guard is a `Bool` the interpreter DISPATCHES on, not a
  `Constraint` beside an extended path.** Instance heads match on the
  generic REPRESENTATION, not on the path, so an erroring path is carried
  along as an opaque type and the next level is demanded anyway — GHC
  unrolls without terminating. `Occurs a seen :: Bool`, reduced to select
  between two instances where the refusing one asks for nothing further,
  halts the descent. See `Tidepool.Form.Check`.
- **Metadata proxies are real `Proxy` constructors, not `undefined`** —
  finding 3 above.
