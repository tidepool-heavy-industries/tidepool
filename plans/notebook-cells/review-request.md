# Initial Astra execution review

Resulting coordinator source: `8dac73c73f8bc51117a070aabe5b9e50864a8c17`.
Baseline extractor-aware `cargo build --workspace` passed. The earlier `just
quick` run was killed with exit 137 after beginning 1,626 tests and is not
acceptance evidence.

## Incorporated readbacks

- Notebook: `plans/notebook-cells/notebook/readback.md` plus checked evidence in
  `notebook/evidence/`. GHC-AST harvesting of a downstream-fixed local binder is
  feasible, and staged execution without a pin demonstrably rejects an expression
  accepted in one `do`. Automatic transport remains unproved: the check module's
  nominal user type is not the later installed `Lib.G<n>` type. The open question
  `inference-transport@9c2b2c0f...` correctly blocks freezing a string-only wire.
- Lookup: authoritative
  `plans/notebook-cells/lookup/execution-readback.md`. One GHC 9.12.2 module resolved a query once,
  enumerated 268 in-scope entries and matched a visible polymorphic value with
  `tcMatchTy`, without candidate compilation. Raw `_` zonks to `ZonkAny` and
  needs parsed-AST normalization to explicit quantified variables. The compiled
  target cannot use an interpreted `IIModule`. Query checking must preserve
  per-query failure; one compile per query is acceptable, never per candidate.

## Proposed release and recursive frontier

Approve lookup for its shared protocol/scope scaffold, then recursive protocol,
hosted-adapter and matching-coverage children. The lookup parent retains matcher
semantics, exact-scope integration, per-query failure, limits and actual
query-to-next-cell proof.

Approve notebook only for a narrow owning-consumer transport experiment plus
independent lexer/layout fixture work. The parent must install a declared type,
pin a downstream-fixed `Response` against that exact `Lib.G` head, consume it,
and repeat across shadowing. It then commits the actual wire scaffold before
forking synthesis/transport and runtime sequencing children. If identity-safe
transport fails, staged-shape checking is a user-visible inference degradation
and returns for product decision; Stage 2 is not silently substituted.

After the notebook wire is checked, recursively fork:

1. extractor lexer/classification and synthesis/diagnostics/type transport;
2. runtime/actor receipts, prefix installation, recovery and terminal `respond`;
3. Generic display eligibility and rendering-tree/`last.more` work after its
   exact typed contract is resolved by the declared fresh Astra slot.

Notebook parent retains the real `ResidentActorRunner::run_workbench_step` join.
Coordinator retains common hosted registration, `status`, common
`documentation_tests`, corpus join and exact-source release review. Corpus work
waits for stable public interfaces. The final consumer declares and binds in a
cell, batch-lookups a name and type, and invokes the returned function in the next
cell; rejection, committed-prefix failure, recovery, shadowing, `respond`, and
pagination are exercised through hosted tools.

## Questions for planner

1. Release lookup broad implementation and notebook's limited transport/lexer
   wave now, with notebook dependent fanout gated on the checked nominal-identity
   experiment?
2. Is any product behavior changed by the readbacks? Recommendation: no. Keep
   terminal middle-`respond` with tail `NotRun`, search only next-cell scope,
   canonical batch lookup input, latest-successful-display `last`, and Stage 1
   inference promise.
3. Any correction to shared-file ownership or the concrete second-level branches
   before the coordinator steers both pending leads?

Release scope remains Stage 1 notebook cells, lookup, display/Generic riders,
`status`, complete corpus migration and hosted acceptance before fresh dogfood.

## Planner decision incorporated

Approved as an asymmetric implementation frontier, not partial product release.
Lookup may scaffold and recursively fan out. Notebook is limited to its
owning-consumer nominal transport experiment plus lexer/layout fixtures; its
parent may release dependent fanout without another planner round only after
checked transport with unchanged contract. Retain the Astra inference specialist.

Corrections incorporated: per-query lookup failure at the worker boundary;
authoritative lookup readback; coordinator ownership of shared `Main.hs`,
ExtractRequest/CBOR, GhcPipeline and actor-workbench seams; demonstrated `read`
ambiguity rather than numeric limit; nominal harvest is not general transport;
preflight rejects unrepresentable transport before effects; and executable lookup
acceptance uses the actual
`awaitSettled :: Response result -> Await (Settlement result)` signature.
