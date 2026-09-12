# Hosted adapter integration obligation

Candidate module: `tidepool-actor/src/lookup_tool.rs`.

The coordinator-owned integration must:

1. add `lookup_tool::declaration()` beside the built-in `haskell` declaration
   in `ResidentInteractivePolicy`, reserving `lookup` against application tools;
2. on a structured `lookup` invocation, call `lookup_tool::prepare`, retain
   per-query `Rejected` entries, and translate valid entries to immutable
   inspection queries;
3. dispatch those queries through the actor's existing exact compile view rather
   than `WorkbenchRequest::for_tool` (which selects compiled Haskell handlers);
4. preserve trusted execution ID/retry correlation and serialize
   `LookupResponse`, with infrastructure failure remaining a tool failure;
5. source the final model-facing description from the shared prompt catalog if
   prompt fingerprint policy requires it, while keeping the declaration/schema
   beside validation or asserting them equal in tests.

Do not accept raw arguments. Do not turn empty or ill-typed individual queries
into whole-batch failure. The actor-side integration test must submit
`[valid name, invalid type, valid type]`, observe all three ordered results, then
use a returned name in the next Haskell submission.
