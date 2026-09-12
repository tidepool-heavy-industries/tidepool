# Notebook narrow-wave integration

Integrated on the planner-reviewed source `a92dd7685818ab6412f24a0a5620b0d8f34e5293`:

* nominal transport evidence from `259197bba6ac7b0e4c954e89b7022b2c88de2af5`;
* GHC lexer/layout foundation from `83a0e75490dd8218fa81f492fd99bd01032a268a`.

The transport experiment checks the two necessary halves—post-zonk
`Response G` harvesting and resident old/new `Lib.G` settlement, consumption,
shadowing, and cross-generation rejection—but not their automatic join.
Dependent notebook fanout remains gated until the coordinator-owned cell
analysis/relocation/preflight/staged-compile seam executes that regression.

## Pragma synthesis decision

The splitter preserves each pragma as its own source item. Synthesis applies
Haskell module semantics rather than inventing adjacency:

* leading `LANGUAGE` and `OPTIONS_GHC` pragmas form the cell-wide generated
  module prologue, retaining source order and source spans;
* a module pragma after the first non-pragma item is rejected at that pragma's
  span before declaration installation or effects;
* declaration pragmas such as `INLINE`, `SPECIALIZE`, and `ANN` remain ordered
  items in the one declaration group. They are not blindly attached to the next
  declaration.

The coordinator's worker request maps lexer `PFailed` through the existing GHC
diagnostic owner. Pest removal remains after the integrated cell consumer and
fixture migration.

## Coordinator-owned patch obligations

One executing integration owner must land the shared seam:

1. opaque `CheckedBinderPin` worker representation with stable item/span/binder
   identity and complete nominal references;
2. preflight relocation of each same-cell reference to the declaration group
   that will become the next exact `Lib.G` head;
3. rejection of incomplete/unnameable pins before any install/effect;
4. staged compile validation against the relocated type, followed by actual
   response settlement and next-item consumption;
5. the old/new shadowing and cross-generation negative regression through the
   production consumer;
6. worker/Main/CBOR and actor/runtime wiring without a second syntax parser.

Only that passing consumer check releases synthesis/transport, runtime
sequencing, and later display/Generic fanout. The splitter itself is ready for
incorporation without changing the product contract.
