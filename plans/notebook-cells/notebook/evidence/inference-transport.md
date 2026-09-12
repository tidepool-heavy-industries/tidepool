# Inference and transport probe

Source inspected: `50a8c17e4ae57df0295c8fae6c68d7b68075977e`.

The fresh Astra consultation typechecked `CellProbe.hs` through the GHC 9.12.2
API, traversed `tm_typechecked_source`, and read `idType` from each post-zonk
`x` identifier. Both binder and use rendered:

```text
x :: Maybe G
x :: Maybe G
```

This establishes that a statement binder fixed downstream to a type declared
in the same synthesized module is harvestable from the checked AST. It also
establishes that `tcg_type_env` is the wrong source for local `do` binders.

Reproduction:

```bash
bash scripts/dev-shell.sh bash -c '
  ghc -package ghc \
    plans/notebook-cells/notebook/evidence/InferenceProbe.hs \
    -o /tmp/inference-probe
  /tmp/inference-probe "$(ghc --print-libdir)" \
    plans/notebook-cells/notebook/evidence/CellProbe.hs CellProbe x \
    haskell/lib .
'
```

The live hosted GHC boundary supplied the complementary staged failure:

```haskell
-- accepted as one expression
do
  value <- pure (read "1")
  pure (value + (1 :: Int))

-- rejected when compiled alone, before execution
probeValue <- pure (read "1")
-- Ambiguous type variable arising from read
```

A same-module declaration group containing `CellVerdict`, `cellScore`, and a
`do` whose `read` result is fixed downstream compiled. After that declaration
was installed, explicitly pinning the staged expression to `CellVerdict`
compiled and `cellScore` returned `1`; omitting the pin was rejected as
ambiguous. This proves the intended pin changes staged compilation and that
the printed surface name can resolve against an installed declaration in the
simple case.

It does **not** prove faithful automatic transport. The checked module's `G`
and a later `Lib.G<n>.G` have different nominal identities. No current wire
relocates the harvested `Type`, and rendering/reparsing does not cover hidden
tycons, skolems, retained constraints, or shadowed prior declarations. The
first implementation scaffold therefore remains blocked on an identity-aware
transport experiment through the owning resident consumer:

1. harvest a downstream-fixed `Response G`;
2. install that cell's declarations as the existing `Lib.G` generation;
3. compile and execute the pinned statement against that exact head;
4. consume it from the next staged statement;
5. repeat with a shadowed prior `G`.

Do not freeze a string-only wire or begin broad implementation until that
experiment either passes or selects a different checked/execution
representation.
