# Engine lane: prepared STG into a simpler production engine

Own M0–M7 from [the engine design](../haskell-engine-stg.md). Start with that
document's recommendation, implementation sequence and completion criteria, then
read the mechanism sections needed for your frontier. Complete the own-words
readback in [the wave allocation](README.md) before implementation fanout.

The result is production prepared-STG execution with less maintained complexity
and lower costs, preserving Haskell semantics and resident effects. Merely adding
an importer or keeping two indefinite production engines does not complete it.
GHC remains the source of prepared semantic facts; Rust owns the execution schema,
validity boundary, machine, memory and effects. Resolve obsolete Core-only guidance
explicitly as the accepted STG boundary replaces it; do not keep a duplicate path
just to satisfy a stale architectural sentence.

## Planned recursive frontiers

1. **M0 evidence and contracts.** Establish the authored engine-review regressions,
   independent GHC reference outcomes, current production consumers and cost/code
   baseline. Record actual outcomes, not watchdog guesses. Agree shared runtime
   error/cleanup and source sequencing contracts with the application lane.
2. **GHC handoff and ownership repairs.** Fork the M1 prepared-STG boundary and M2
   independent failure/external-storage ownership work after their shared facts
   are established. M1 proves the actual GHC pass handoff; it must deliver enough
   concrete representation evidence to constrain the schema, not a guessed API.
3. **M3 execution contract.** Lead integrates those results and scaffolds the small
   recursive execution schema and Rust construction invariants. Once exact facts
   and encodings agree, Sol subtrees can own Haskell projection/encoding, Rust
   validation/linking and reference semantics. Check their shared fixtures and
   real consumers together before the next frontier.
4. **M4/M5 code and memory.** Freeze an accepted calling/layout/root contract, then
   open useful code-generation and heap/layout/collection subtrees. Within codegen,
   split independent function/join/application and thunk/control obligations only
   after their ABI and failure behavior agree. Precise GC and emitters share a
   concrete safepoint/root contract; neither can invent it independently. Integrate
   smaller working execution paths and awkward semantic cases progressively.
5. **M6 production cutover.** Lead owns the actual extractor/toolchain/artifact/
   runtime consumers. Fork bounded consumer migrations from the checked engine
   baseline, consolidate, remove the old production path and verify generated
   fixtures. Coordinate changed runtime consumers and release inputs with the
   application lane before overlapping edits.
6. **M7 measure and simplify.** Compare the accepted implementation against M0 on
   the specified paths and close the design's concrete optimization/deletion
   obligations. Measurements choose remaining changes; no ornamental optimization
   framework. Integrate/check every accepted simplification through production.

This is an initial decomposition for the lead to refine, not a stage-worker zoo.
The lead does substantial implementation and successive integration work. Scope
children around coherent semantics and consumers; reuse shared context where its
reasoning is relevant and selected context for independent review.

## Declared Astra consultations

- **STG and execution schema:** after M1 evidence, a bounded expert can settle the
  consequential GHC pass/schema question before M3 freezes its consumers. Include
  specific GHC facts, examples, proposed representation and actual uncertainties.
- **Calling, roots and failure:** at the M3→M4/M5 seam, a bounded expert can resolve
  calling/layout/GC/control-flow correctness with representative normal and failing
  programs, current owning code and proposed invariants.

Use `DesignSlot` / `consultDesign` with `gpt-6-astra`, ordinarily High. Expert
results are concrete decisions/amendments for Sol to incorporate. Consult only
when the frontier needs it; do not load an expert with routine coordination.

## Acceptance and reporting

Apply the engine design's supported-contract matrix, deletion ledger, performance
obligations and completion criteria. Compile changed consumers and run focused
semantic/failure cases in the owning Nix setup. Translation/serialization changes
require the fixture boundary check. Broad checks occur at integration points.
Report independently observed outcomes, compilation versus execution, source,
production cutover/deletions, measured changes and remaining M0–M7 gates.

The running swarm's extractor, embedded library and machine remain frozen at the
launch baseline. Candidate compiler builds and tests must use explicit candidate
toolchain selection; never redirect the running swarm's compiler socket or mutate
its installed `.shoal`. Live candidate acceptance belongs at a later explicit
swarm boundary after matched application integration.
