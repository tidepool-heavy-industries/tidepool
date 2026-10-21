# C2 demand report (summary) — kind/RuntimeRep metadata in the package product

Role C2, branch `g2-deepseek/c2-20261021`, base `0cfb964739`. Read-only; no
compile, no implementation. Full report with line references:
`/srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/c2-demand/REPORT.md`.

## Outcome

`selectPreparedEvidence` filters a module-wide type graph (built once in
`PreparedSites.acquirePreparedSitesTracked` for every site) down to the
selected group's sites; `lowerPreparedEvidence` then lowers the selected graph,
auxiliary-root answer types and constructor replies into `projectedTypes`
(TPGRP-v2 field 15) and `programConstructorReplies` (field 17). Kind and
`RuntimeRep` metadata enters the product at five `Tidepool.TypePolicy` sites:
declaration binder kinds, primitive `ScalarDeclaration rep`, constructor
`Field { ordinal, source_rep }`, `ForAll` kind edges, and function
multiplicities.

## Demand table (compact)

| # | Requester | Carried data | Actual consumer | Unnecessary work | Earliest cut owner |
|---|---|---|---|---|---|
| A | `PreparedSites` (all sites interned) | module-wide `TypeGraph` | `selectTypeGraph` reachability filter per group | types of unselected groups interned then dropped | `PreparedSites`/`PreparedStg` (selection-driven build) — hypothesis |
| B | `TypePolicy.internDeclaration` | `TypeBinderKind` kind DAG | `TypeGraph::validate` scope/cardinality + `declaration_identity_eq` | kind content never read by execution/rendering, only compared | `TypePolicy` — hypothesis (needs C1 + oracle) |
| C | `TypePolicy.constructorTemplate` | `Field.source_rep` | `validate_roles` cross-check vs `ConstructorDecl.field_reps`; hash; identity | exact duplicate of field 11; runtime reads `ConstructorDecl` via interner | `TypePolicy` / `lowerSelectedTypeGraph` (derive at validation) — strongest |
| D | `TypePolicy.internExpression` | `TypeKind` (ForAll), `TypeMultiplicity` | `contains_effect` (Kind/Body), identity | multiplicity compared but never interpreted | `TypePolicy` — hypothesis |
| E | `TypePolicy.internRoot` | `Root.rendered` string | conflict/refusal diagnostics only | serialized but identity/evidence-excluded | `internRoot` / `ExecutionEncode` (drop or side-channel) |
| F | `validatePreparedEvidence` | auxiliary-root answer types | `lowerAuxiliaryRootEvidence` | empty graph for every package group (`__resume` lives in home module) | `validatePreparedEvidence` (home/executable only) |
| G | `lowerConstructorReplies` | `(ConstructorId, ConstructorReply)` with `TypeNodeId` root | runtime resume re-opens the answer type (`PreparedFrameEvidence`) | none — execution- and contract-required | keep |

## Confirmed vs hypothesis

Confirmed: all five entry points; `Field.source_rep` equals
`ConstructorDecl.field_reps` at both producer and decoder; execution/rendering
never read `BinderKind` or `Field.source_rep`; contract validation does traverse
them; `Root.rendered` is diagnostic-only; package-group auxiliary scans are
empty.

Hypothesis: C (drop `source_rep`, re-derive at validation), B (thinning or
dropping binder-kind comparison in favor of `TypeVersion::GhcAbi`), A
(selection-driven graph construction preserving freeze-before-issue), D
(multiplicity content). Byte-size savings are unmeasured; whether the duplicate
envelopes differ in field 15 at all is C1's equivalence question, not this
trace.

This report changes no source and does not merge into the campaign. P's exact
occurrence linkage remains necessary regardless of any C cut.
