# World observation context

The existing compiler World lexical product retains an optional protected observation scaffold. It is descriptive compiler input, never a runtime grant. Module-only Worlds remain valid and refuse row-dependent questions with a typed unsupported result. Queries do not fall back to the finite inspection batch.

## Issuing owner and lifetime

The common `ActorWorkbenchSource::for_workbench` source producer issues the scaffold from the actual admitted actor context and current `RequestWorkbenchScope`. The same producer supplies the cell recipe and observation declarations. It retains the original static source preamble, ordered static imports, effect alias/type imports and actual request/input TYPE helper declarations. It excludes cell bodies, an arbitrary previous cell recipe, handler executables, runtime input values, request-ID VALUE right-hand sides, and the runtime `me`/`currentModel` right-hand sides. Do not recover declarations by stripping arbitrary Haskell text.

The closed profiles are Workbench and HostCarrier. A HostCarrier's pure row must never silently replace a Workbench row. Request signature slots use the existing native checked-signature producer; display strings are not type evidence. Observation helpers have signatures and inert bindings suitable only for private compiler observation. They are not evaluated or published.

The scaffold is retained once in `CompilerWorldOpenInputs`' existing lexical surface, then decoded once into Haskell `WorldInputFacts` owned by `HeldCompilerWorld`. Mutable GHC realization state stays in the existing `WorldPreparedModules` owner. Per-query requests contain only the held World identity/realization and task operands. `TcGblEnv`, `HscEnv`, runtime values and repeated full context snapshots do not cross this boundary.

## Identity and sharing

Changed compiler semantics (selected imports, row, protected declarations, nominal reply/progress/input types or profile) changes the lexical fact and logical World. Different request IDs with identical compiler semantics may share the same logical World. Exact request identity remains in the actual actor pin, source admission and settlement revalidation; it is not another compiler fact field or an executable literal in the scaffold.

A scaffold-only change derives a new lexical root/product through the existing World owner while sharing primary context, primary associations, source capture, binding rows and their immutable custody. It must not recapture source files or enumerate all retained values. Existing World acquisition/restoration and source/authentication/END checks remain authoritative. An equal scaffold returns the same World reference. A changed scaffold clears its checked-cell position; it cannot inherit another logical World's item certificate or ordinal. The actual actor scaffold must be attached before plan/reserve and inherited by item successors; do not install a changed observational World over an active checked prefix.

## Paired lexical record

`TPCWLEX2` is an array of four values:

1. `TPCWLEX2` tag.
2. Existing ordered current import specifications.
3. Existing selected declaration facade record or null.
4. Null for module-only, otherwise `[profile, protectedFragment, staticImportSpecs, requestTypes]`.

Profile is 0 for Workbench or 1 for HostCarrier; other values fail closed. `protectedFragment` is one exact source fragment, not independent editable preamble/row/helper strings. `staticImportSpecs` preserves order using the existing import-spec owner. `requestTypes` is null or `[request, input]`: request is null or `[helperRecipeTag, RequestTypeSignatures]`; input is null or the existing CheckedSignature CBOR value. Tags and nested values use the existing checked-signature codecs (`none` / `actor-reply`). Actual leased HostBindingPrototype/checked-carrier type ownership supplies input, never BoundBinder text. Haskell uses `rewriteRequestTypes` with those exact native Types and checks the existing generated signature slot counts. Legacy lexical tags and malformed/unknown variants fail closed. The existing product root slot remains unchanged; its CID commits the new lexical body.

## Observation and acceptance

The Haskell World owner hydrates the exact selected lexical facade and validates exports before deriving private observation context. Keyed rowFit resolves one candidate in that actual context (preserving aliases/reexports, namespace, ambiguity and exact generation), then uses the existing GHC row solver. Expressions and type search use the same held context. Private probes do not advance a cell prefix, publish a successor, install names or grant runtime execution.

Acceptance requires actual current-scope positives through the production effect loop, wrong-row and changed-request-type negatives, stale settlement and module-only refusal, and a no-publication assertion. Same-semantic request-ID changes must preserve the logical World while changing the request-bound actor observation pin. Allocation-only reservations and transport relocation preserve semantic pins. Local codec/shape tests are not all-five or installed-Persona acceptance.

## Remaining implementation boundary

The native input-type helper comes from the actual checked carrier/prototype retained while mounting the leased input. The read-only accessor exposes existing signature data, not fresh permission. RequestTypeSignatures supplies reply/progress types only. Generated input slots use the separate retained native CheckedSignature. No rendered input type or runtime mounted input value completes the scaffold. Q owns the Rust fact/capture and actor route; H owns paired lexical decoding and the GHC observation producer. The current joined source has no complete scoped observation producer yet.
