# C: one root-scoped import admission owner

Accepted implementation contract, 2026-10-22. Base
`1c56d053aaddfad3177105cae33e897389e437ad`; integrate on
`refactor/g2-linked-code-ownership-20261022`. G2 and G3 remain unqualified.
This supersedes the earlier local census repair and multiset proposal.
Internal APIs may break; Rust protocol4 and Context9 remain unchanged.

## Mechanism

Captured generated-source coordinates are valid in their captured buffer.
GHC's in-memory target preprocessing prepends a LINE pragma. The later parse
therefore cannot consume the raw census positions. Pinned GHC BufPos counts
codepoints, so Unicode is a control, not a confirmed unit defect. Exact failing
numeric span pairs were not retained; exercise the actual driver in acceptance.

The pipeline also reconstructs protected imports from line numbers/prettyprint,
reclassifies origins and repeats Finder resolution after completion. Replace
those parallel decisions with one carried owner. Preserve early TYPE/source
qualification, actual-consumption validation and physical HPT checks, which
establish different facts.

## Shared module and type contract

- `ImportSyntax`: structural descriptors, occurrence IDs, captured/driver
  correspondence and lineage DATA. No source, TYPE or native authority.
- Private `ImportSyntax.Internal`: representations used only by actual syntax
  and authority issuers. No public generic origin retagger.
- `ImportAdmission`: opaque public stages and their read-only projections.
  Hidden `ImportAdmission.Internal` contains trusted issuer representations.
- Private GhcPipeline transitions invoke the existing scope, source and World
  validators and immediately issue the next stage from their actual results.
  They do not accept a detached source selection or an authority-asserting
  callback. Keep those validators beside their compiler operations; enforcing
  the stage contract does not require moving all their policy dependencies.
- Compiler environments are borrowed during transitions. Retained admission
  stores neither HscEnv nor a whole dependency graph. Audit imports of Internal
  as the narrow trusted construction boundary; public callers cannot update it.

| High-level stage | Evidence |
|---|---|
| Captured | Immutable root/source, ordered occurrences, issuer-derived origins, effective parser/purpose policy. |
| Scoped | Actual demand-selected early prerequisites from finite or World issuers; not driver selection. |
| Matched (private data) | Ordered structural correspondence to the actual root parse and captured origins; no selected-owner claim. |
| Resolved | Actual selected owners, resolved after the frontend installs its exact source-less interfaces, and prerequisite reconciliation. |
| Prepared | Certified transformation lineage paired inseparably with the exact NativeParsedModule entering typechecking. |
| Completed | Compact actual consumed decisions and terminal projections; no Finder. |

Private constructors/update labels; each transition consumes its predecessor.
Reuse existing request/stage/attempt/source identities. Carry exact immutable
references, not complete snapshots at every stage. Do not spread generative
parameters into persistence. Ordinary-authored and generated ingress are
explicitly different; absent generated evidence refuses. Every generated
auxiliary with special origins has its own root admission.

Import correspondence proves only the import sequence. The actual driver parse
must also belong to the captured target contents, full Module identity, source
path, GHC summary fingerprint, effective policy and request/stage/attempt.
Parse once after the actual downsweep: protected-source qualification needs
that result before planning/loading. Retain it in the private Matched driver;
early Finder resolution would precede selective source-less interface hydration.
Borrow this same parse for quotation policy and current-source qualification.
Resolve actual owners after the frontend's prerequisite installation, then
reuse the owned driver through all three frontend routes. Check full Module,
source kind, path, source hash and effective source policy. A boot summary is a
different input from a regular summary of the same Module. Backend/output policy
may change during planning; private summary rebinding preserves the parsed body,
imports and occurrence lineage.
Delete discarded early parses and later reparses rather than postponing required
qualification. Prepared owns the exact refined input; actual target typecheck
success is retained privately until the whole compilation succeeds, then issues
Completed. An earlier root receipt is pending and cannot feed a terminal writer.
The target cannot borrow a previous attempt's completion; dependency reuse stays
separate. Reusing an immutable recipe in a fresh attempt remains valid; mixing
issued stages from different attempts does not.

The lower AST bundles are only `DriverImportSyntax` and
`RefinedImportSyntax`; do not create a second Scoped state lattice.
Driver correspondence precedes any refinement. High Prepared invokes the actual
sanctioned refiner on its own Resolved driver and owners; a refined data value
alone grants no authority. Never pair an arbitrary replacement AST with lineage.
Lift ExactCompilation and its root-specific finite receipt adapter above
ExactScope if needed; leave lower validators on narrow existing inputs.

## Origins and transformations

Authored originates in the actual parsed submitted prologue; Inherited in the
actual completed lexical owner; Protected in the captured admitted template.
Hide LocatedImport constructors/intent updates and generic origin-to-string
rendering. Keep read-only projections and evidence-specific producer operations.

Raw-to-driver matching is ORDERED complete structural equality, excluding
cross-buffer spans, followed by ordinal correspondence. Preserve module,
package qualifier, SOURCE/safe flags, qualification style, alias, implicit bit,
namespace and complete import restrictions including wildcard position.
Unsupported forms refuse. Prettyprinting and names alone never carry authority.

Identical imports keep distinct occurrence IDs. Protected P cannot exempt
identical Authored A. Union roles only in the terminal edge receipt after both
obligations pass. Reordering needs a certified transform, not a multiset fallback.

CheckedPrefixImports narrowing and qualified clones carry their parent through
the operation itself. Preserve original selection and alias on qualified clones.
Synthetic Prelude has explicit lineage and its actual selected owner with normal
qualification. Native annotation maps must preserve imports. Unknown changes
cannot yield Prepared. World no-execution checks stay early; authorized finite
TH remains supported.

## Delete the old routes

Remove ParsedImportSelection, withSourceImportIntents and pvSourceImportIntents
after migrating consumers. Delete repeated census, protected line/shape matching,
duplicate target parsing, post-completion Finder and generated-proof fallback.
Finite TPEXACTCOMPILE/4 and World TPCWSOURCE4 writers become projections.
Emit sorted unique (qualifier,module,boot,resolvedUnit,mask) HOME rows, masks
authored1/inherited2/protected4. Preserve exact source-search alias spelling.

Rust still checks canonical consumption separately from lexical permission.
TYPE and body custody do not grant SOURCE. Inherited scope must actually exist;
protected dependency adjacency must not publish unrelated lexical roots.
Legitimate historical issued dependencies remain valid alongside newer bodies.

## Parallel corrections

Preserve World query failure constructor/phase/detail in bounded, filterable
structured logging before coarse projection. Do not catch cancellation or
replace the original result. The instrumented carried-World query at `ade778a5f1`
reported the same parsed-source origin-census failure as the staged histories;
no separate carried-TYPE defect is established by that failure.

Fix obsolete native-entry error assertions while preserving four mutations,
later checks and no-state-growth. Rebuild publication fixtures through real
owners: Full availability with explicit Empty demand, Full positive and Empty
refusal. Inspect demand on the actual prepared request, then consume it through
target admission: a body-support query must remain true for both modes and does
not authorize execution. Empty must refuse with the exact root-choice cause.
Do not invent UnissuedPolicy. Historical tests retain A, select B,
introduce unrelated interface-only C, and refuse A as a substitute for B while
allowing an actual historical A request. Do not assert actual TH canonical
equality; existing component controls own same-canonical alternative bodies.

## Assignments and joins

| Owner | Owned work |
|---|---|
| Root | Shared contract, bounded diagnostic seed, critical review and integration. |
| S: g2_package_demand_binding | ImportSyntax/Internal, TurnSource, Binders, capture-only ExactHydration. |
| D: g2_demand_closure | CheckedPrefixImports, directly needed CheckedCell wrappers; lineage. |
| P: review_package_selection | ImportAdmission, GhcPipeline exclusively, higher ExactHydration authority extraction, finite/World adapters, TypedSegment/Source, Main and PlannedDeclaration callers. |
| N: review_n_frontier | Existing Haskell actual-driver controls and independent properties. |
| A: g2_actor_custody | Exact native-entry refusal tests. |
| H: g2_custody | Production publication/reuse and historical fixture premises. |
| V: g2_validation | Exact joined pins, warm compilation and counted execution. |

Root also owns `TypedSegment/Source.hs` during the C join. Its demand proof now
consumes the opaque generated-import capture instead of parsing and classifying
the source again. N owns directly affected test callers; P owns Pipeline callers.

Use isolated source branches. Communicate shared API changes directly.
Only P edits GhcPipeline after root's seed commit. Current scratch is about
119 GiB; preserve 100 GiB and avoid multiple cold expansions. Warm joined
compilation is available as coherent commits land. No test pass from linking.

## Acceptance sequence

1. Compile lower then joined consumers early. Retain first failed action/stderr.
2. Real targetContents=Just preprocessing/admission with LINE, Unicode,
   multiline/comments, duplicate Protected/Authored, extra/missing/reordered
   occurrences, changed source/attempt and unchanged retry. Include actual
   permission refusal, not merely a mask encoder. Exercise certified clones,
   synthetic Prelude and unregistered transformations.
3. Staged1, priorWorld1, mixed1, carried-World, publication and native-entry.
   Diagnose any changed later failure from its actual cause.
4. Real inherited-vs-TYPE, missing/extra HOME and narrow publication histories.
   Independent generated correspondence/lineage properties complement them.
   Count origin classifications and completion Finder calls (one and zero).
   Do not infer wall-clock speedup from structural savings.
5. Existing G2 actor/notebook/reload/refusal/custody/Context9/corpus gates,
   deletion audit, map refresh, consolidated pushed checkpoints.
6. Freeze a matched bundle, verify real Persona tools and rendered memory,
   start fresh DeepSeek preserving authored files. G3 broader recovery/perf
   remains separate.

Base evidence and exact rerun commands:
`/srv/swarm/evidence/inanna/g2-sol-tranche-20261022/validation/NEXT-ASTRA-VALIDATION.{md,json}`.
Use fresh output paths. Full-parent custody reduction and historical compiler
root cleanup stay separate; neither silently expands C.
