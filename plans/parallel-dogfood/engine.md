# Engine lane

Own M0–M7 in [the engine design](../haskell-engine-stg.md). Start with §1
(recommendation), §11 (sequence) and §14 (completion); follow the mechanism
sections as needed. First deliver the
[plan in your own words](coordination.md#check-the-plans-before-implementation) before implementation.

The outcome is a smaller, correct, cheaper production engine using prepared STG.
An importer alone does not complete it. The detailed plan owns semantics and
acceptance; resolve stale Core-only guidance explicitly at the accepted cutover.

## Initial decomposition to refine

| Frontier | Scaffold → useful forks → integration |
|---|---|
| M0 | Independent GHC/reference/JIT outcomes, regression and cost baseline; agree runtime failure/cleanup seams |
| M1 + M2 | Fork real GHC prepared-STG handoff and independent failure/external-storage ownership repairs |
| M3 | Integrate GHC evidence; agree execution schema, then fork projection/encoding, Rust validation/linking and reference semantics |
| M4 + M5 | Agree calling/layout/root/failure contract; fork codegen and precise heap/GC work, subdividing only after shared ABI exists |
| M6 | Fork production consumer migrations from the checked engine; integrate actual cutover and delete the old path |
| M7 | Measure specified costs against M0, simplify, integrate and close remaining design obligations |

Each subtree may repeat scaffold/fork/integrate; the lead implements and owns
substantive joins. Emitters and GC must share a concrete safepoint/root contract.
Coordinate runtime/session consumers and release inputs with the application lane.

## Declared Astra slots

- After M1: consequential GHC pass/schema decisions before M3 consumers freeze.
- At M3→M4/M5: calling, layout, roots and failure correctness.

Use `DesignSlot` / `consultDesign`, Astra High, with actual examples and bounded
uncertainty. Sol incorporates the decision and continues implementation.

Acceptance includes the supported-contract matrix, deletion ledger, production
cutover and measured cost obligations. Use focused Nix-backed checks and the
required translation/serialization fixture boundary. Record observed outcomes,
not watchdog guesses. Candidate compiler tests use explicit candidate selection;
never redirect the running swarm's extractor or compiler socket.
