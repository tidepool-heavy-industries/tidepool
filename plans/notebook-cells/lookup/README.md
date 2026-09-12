# Lookup lane

Independent of cell sequencing; start against existing live inspection scope.
Read session/inspection.rs and Tidepool/Introspection.hs and their actual consumers.
Parent owns scoped GHC integration and end-to-end hosted lookup.

First proof before parser fanout: assemble current scope, resolve one :: query to
a GHC Type, enumerate visible candidates, match without per-candidate compilation,
return a usable function and invoke it in the next submission. Verify setContext /
typeKind feasibility; don't retire unrelated existing paths without evidence.

Then share query/result types and realistic query fixtures, and fork query/batch
dispatch/presentation versus matching/scope coverage. Coordinator owns common tool
registration; provide its exact interface before consumers fork.

Use GHC syntax/type machinery rather than a second Haskell type parser. Fresh
Astra slot if matching semantics or interactive resolution requires design.
Search only what next cell can name; exclude shadowed/unimported generations,
verify imported exports enumeration, disambiguate qualified names and label live
bindings. No per-candidate compile. Test query isolation and usable polymorphism.

Wildcard parsing is not wildcard matching. Test repeated named variables versus
independent anonymous holes. AST-level replacement with fresh variables is a
candidate strategy, not authorized text substitution or a verified algorithm.
Exact matches first; deterministic bounded results with actionable truncation.
Use existing declaration rendering if Haddock unavailable. Canonical queries list;
raw string convenience only if real hosted transport supports it.

Readback includes the failing-today query -> returned function -> actual invocation
fixture, concrete results for ambiguous name/batch partial failure/wildcards,
shared source and recursive frontier, retained parent engineering and checks.
