# Import and retained-ownership review checkpoint

Branch: `integration/g2-domain-runtime-20261023`.
Checkout: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Code candidate: `75a0aa0fed3e30745ff43053d9ed5ea1a0f7cd72`.
Review base: `35066c2115040ce9cf462d8ade764cb2ef84a4fc`.
Later commits change guidance and this handoff only.
Evidence root: `/srv/swarm/evidence/inanna/g2-import-ownership-closeout-20261024/`.

The accepted A/B/C repairs are integrated. All three joined history cases now
pass. This is a regional execution checkpoint for external Argon review, not
G2/G3 qualification. No frozen bundle or fresh Persona/DeepSeek run was made.

## What changed

### A: declaration selection and demand share import semantics

`CheckedPrefixImports.refineProgramDeclarationImports` receives the chronological
previous facade plus current-cell Originals, preserving grouped export identity.
Wildcard admission reads immutable captured syntax, then follows its exact live
occurrence. Nonrecursive occurrence roots make repeated narrowing update the
existing descendant without adding another qualified clone. Record fields keep
their actual parent identity. Separate Original/program wrappers are deleted.

Joined execution then exposed another consumer of those imports: demand renaming
ran before final frontend refinement. At `1519173f26`, G5 completed successfully,
but the next segment's demand probe still saw raw G3/G5 imports and refused an
ambiguous `historyCaptured`. This is separate from the repaired stage-9 wildcard
premise failure; the intermediate failed attempts are retained.

The World pipeline now uses one import-only recipe for demand and final checking.
`projectRefinedDemandImports` binds the projection to the actual generated
driver, captured bytes/path, full module, source hash, parsing policy and captured
occurrence census. It replaces only imports in the original demand proof.
Declarations, native annotations and the no-quasiquote/root checks survive.
`moduleImportRequests` always reads the immutable source capture. Final segment
lowering and completed-value aliases stay out of demand; final import admission
still resolves and checks actual selected owners.

### B: exact target/stage/slot provenance reaches runtime authorization

`ExactCompiledItem::select_cell_stage_retained_input` consumes an opaque slot
witness whose target and origin are private. Sealed stages retain their own
completed physical-product receipts. CurrentStage can select its incoming value
or a sealed EarlierItem from that stage. PriorProgram requires exact earlier
physical issuance and explicit receiving-stage selection; it has no EarlierItem
fallback. PriorStage joins the actual producer item and checked row; runtime
Produced evidence requires that row's Arc identity. Historical terminal and
item/admission/target/slot, lexical lease and installation checks remain intact.

This uses the existing completed receipt owner, without retaining a complete
compiler stage or adding a second runtime value registry. The leaf-only API is
deleted. Item indices and stage-first-item ordinals retain distinct meanings.

### C: materialization retains issued rows

`CertifiedNativeGroupBaseline::Rows` borrows actual issued Arc rows. Materialize,
detach and request assembly preserve them and their ordered imports. One fallible
selected-row collector refuses conflicting issuance for requested bindings before
custody reduction; unselected alternatives and physical-file precedence remain
legal. Physical equality and complete selected issuance remain distinct.

Two valid-fixture witness surgeries now use the production typed receipt issuer.
The test-only map helper moved inside tests, and the redundant fixture certifier
was removed. The strengthened journey independently expects HOME Source followed
by Package, and checks order and Arc identity through each consumer.

## Executed evidence

`joined-acceptance.json` retains exact reports, hashes, executable identity,
source pins, elapsed times, counts and earlier failed attempts.

| Check | Executed outcome | Evidence |
|---|---|---|
| Deterministic cold → repeat → rejection → recovery | 1/1 pass at `75a0aa0fed` | `joined-75a0-deterministic-serial/` |
| Distinct current value plus qualified historical value, then another cell | 1/1 pass at `2a7f9766c6` | `joined-2a7f-history-three/af7aeb8565ba408173b32b42fd7621d87e6da16cedb4a213dbe50975afcad7d6.json` |
| Bounded generated GHC-differential histories | 1/1 libtest pass, 3 reported histories at `2a7f9766c6` | `joined-2a7f-history-three/623b954119de0cdac458404d45d563f104c7b313a176b882131ae4f46f3b5113.json` |
| Final Haskell demand/import controls | 5/5 named cases; worker and 3 affected test binaries compile | `imports/demand-summary.json` |
| Earlier Haskell import/adjacent consumers | 6/6 named cases; overlaps final batch, not additive | `imports/summary.json` |
| Retained ownership | Physical 1/1, runtime 7/7, final prior/historical rerun 1/1 | `slots/evidence.json` |
| Issued-row controls | 9/9 plus strengthened ordered-import rerun 1/1 | `rows/evidence.json` |
| Selected joined fixture cohorts | Recovered-base 1 program; user-types 8 programs pass | `rows/joined-fixture-evidence.json` |
| Integrated build registration | Generation check and 61 script tests pass | `integrated-generator-{check,tests}.log` |

`2a7f9766c6..75a0aa0fed` changes only Haskell test source. Production code and
the runtime histories are identical. The joined runtime reports confirm process
and compiler cleanup, but report `diagnostic_evidence_complete=false`; do not
describe their diagnostic capture as exhaustive. Timings are workload outcomes,
not a controlled performance comparison.

The distinct-value history independently requires `[23,23,91,91,23]`, then 91
from a later cell. Repeating equal inputs alone would not detect a stale winner.
The generated case compares actual observations with GHC and checks mandatory
capture/shadow, unused-bottom, rejection and recovery coverage.

## Review questions and limits

1. Challenge the shared import-only boundary, including the source-policy chain,
   qualified/restricted imports, and preservation of no-execution evidence.
   Demand must not become prepared import admission or completed-value authority.
2. Challenge Current/Prior/Historical authorization and exact producer-row joins.
   The tests separately exercise same-stage and prior-declaration captures.
   They do not establish prior executable reuse with a capture from its original
   stage. Thin value interfaces and excluded executable scaffold owners prevent
   the obvious ordinary notebook construction. This combined positive and the
   independent PriorProgram-without-receiver-selection refusal remain unverified;
   see `slots/prior-executable-same-stage-reachability.json`. No fixture-only
   authority was introduced to manufacture those cases.
3. Check issued-row preservation and selected-only conflict scope, including
   nonempty ordered imports, unselected alternatives and file custody precedence.
4. Internal reviews found no remaining scoped production issue or superseded
   entry point in these repairs. Their source reviews are not execution evidence;
   the table records actual execution separately.

## Independent startup issue

The first parallel deterministic attempt at `2a7f9766c6` failed before notebook
execution: Nix `--query --roots` reported `dnix/store/...`. The other two cases
passed; the batch therefore exited 1. Its later serial deterministic retry
passed. Both results remain recorded, without an automatic hidden retry.

The pinned Nix 2.31.3 scanner contains a strongly matching race: one scanner
unlinks a stale temporary-root inode and writes `d` at offset zero; another
scanner can read that marker as part of the old path. The actual competing
process is unidentified. These queries scavenge stale roots, despite looking
like read-only observations. Both failed attempts confirmed preboot cleanup.
No root verification was bypassed and no system configuration was changed.

See `nix-roots-startup-investigation/report.txt` for exact upstream provenance,
cleanup evidence and repair options. The same investigation found a reporting
gap: useful stderr is retained in a file, while the exception's stderr field is
empty. A controlled isolated-store reproduction and an owned Nix repair are
separate work; local scanner serialization would not cover external scanners.

## Next gate

External Argon reviews this candidate and the exercised/refused boundaries.
After acceptance, follow `plans/shared-warm-compiler/completion/g2-completion.md`
for ordinary notebook/discovery/reload/lifecycle checks, the matched frozen
bundle, selected Persona tools/rendered memory, and bounded real DeepSeek use.
Parallel service startup is not qualified by the serial retry. G3's broader
force/fork/recovery and performance obligations remain separate.
