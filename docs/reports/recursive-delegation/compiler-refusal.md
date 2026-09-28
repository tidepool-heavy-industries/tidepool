# Prepared home-body refusals during recursive-work checks

The focused `Project.CheckedReviewChecks.sourceMismatch` recipe failed while
preparing its setup cell, before the source-mismatch assertion. Definition ID:
`e4813b6879d633cad537c06cd46b752e7a951a28f6e9360e78f04ca857806bb4`.

The diagnostic names missing home implementations of
`Tidepool.Actor.Record.$fGActorapiK6` and `$fGActorapiK7`; projection reports
`MissingPreparedTop` for the former. The setup cell uses the same public
`startReviewFlowWith` composition already exercised by the passing publication
and bounded-repair cases. The refusal happens during compilation, before the
scenario-specific check runs.

Retained failure artifact:
`target/tidepool-test-runs/20260928T040658Z-1803819-exomonad-check`.
The supervisor's `/tmp/recursive-Project.CheckedReviewChecks.sourceMismatch.log`
includes the complete cell and error. The compiler request was
`e0713ca037bf1b6e`. The daemon subsequently rotated its worker automatically at
7,253 MiB against a 7,168 MiB ceiling, after 63 requests. This timing does not
establish memory pressure as the cause. No daemon was manually restarted.

The owner is prepared home-graph recovery/projection, not the model-facing
review verdict. Recovery explicitly refuses absent home bodies rather than
substituting stale interfaces. A useful investigation compares the current home
module's prepared binders with the references reaching it, including cached
interface/prepared-body identity across requests. Do not weaken that refusal.

A reproduction against the unchanged isolated workspace copy passed both
assertions and completed cleanup, with the same definition ID. Retained success:
`target/tidepool-test-runs/20260928T040942Z-1819740-exomonad-check`;
log `/tmp/recursive-sourceMismatch-repro.log`. The failure is intermittent; no
compiler repair is claimed. Preserve this as an engine investigation card rather
than weakening review source checks or hiding the first failure.

During the reproduction's final cell the worker again crossed its memory ceiling;
request `1f0b4f13de35359f` took 68,589 ms immediately after automatic rotation.
This is measured cold-worker latency, distinct from the dictionary refusal. It
explains the long final-cell wait in this check, not all notebook latency.

## Exact-source review admission

`Project.RecursiveWorkChecks.revisedReview` twice refused
`Tidepool.Actors.Unfold.validatedLiteral` during the first `requestReview` call,
before any assertion. Both attempts used definition
`17b8ea28b6b7f441e2494f7b0b073f0bb977f51a11cbe4d736b97cb04b10de3b`.
Requests were `c0d55ccbbcadd66b` and `c62759b0d0ad1905`; retained runs:

- `20260928T041340Z-1843145-exomonad-check`
- `20260928T041658Z-1861447-exomonad-check`

The existing `Project.SkillChecks.reviewProvenance` control passed all twelve
assertions on that same definition, including `reviewCommit`'s equivalent
path-label construction. The private `validatedLiteral` implementation backs the
public `ForkGroupLabel` IsString instance; it should be reachable normally.

The three project review entry points now share one private `admitReview`
implementation, removing duplicated admission policy. After that source change,
`revisedReview` passed all four assertions on definition
`82fbd3a4419f9c05ae4f1ed9b8472e02e030e90d1fced7a6a4acfb65f4cfba40` in
`20260928T042340Z-1899071-exomonad-check`. No engine refusal was weakened and no
compiler source was changed. This establishes that the published composition
executes; it does not establish a repair of the compiler's missing-body cause.
Retain the earlier source/log evidence when investigating interface/body identity
and reachable home binders. Do not attribute the fix to a cache flush or memory
pressure without a controlled reproduction.

## Runtime case trap in canonical admission

The first published fork cell intermittently fails after an effect response with
an integrity `CaseTrap`, before the first recipe assertion. It constructs a
`Branch ... (Outcome Candidate)`, calls `unfoldWork`, and retains the original
handles from `batchMembers`. Literal tuple-name patterns were removed as needless
comparisons, but the wildcard form also failed; that edit is not an engine fix.

Failures include runs `20260928T044310Z-2000108-exomonad-check` and
`20260928T045409Z-2056175-exomonad-check`. The latter uses definition
`c92c7ea18c5305987735901f493790c5d88bdf4e1052b0f28ffdac9de3581c08`.
An unchanged-definition diagnostic rerun
`20260928T045646Z-2067676-exomonad-check` passed admission and twelve assertions,
then exposed an unrelated stale workbench example. Logs are retained in
`/tmp/recursive-skills-final.log` and `/tmp/recursive-skills-trap-diagnostic.log`.

A read-only engine consultation found no invalid Haskell composition. The fatal
trap means an unrecognized/nonconstructor scrutinee or an impossible empty/
nonalgebraic case; a recognized unexpected constructor produces `CaseMiss`.
"After delivering a response" names an effect resumption, not child settlement.
Candidate fields remain pending at this point; WorkSink is an erased newtype.
Useful diagnostics are the last resumed effect, compiled owner/node, expected
case family, and actual object kind. In particular distinguish ActorStartWith's
host-answer triple from the applicative/configuration pairs around admission.
The runtime's claim that diagnostics are on stderr is currently misleading.
No case-trap or home-body compiler fix is claimed by this implementation.

## Recursive fixture on inherited-context source

The strengthened nested fixture selects fresh root-to-Luna context and inherited
context for deeper Luna work. Its first attempt failed in the root's initial
batch, before reaching inherited admission, with missing home implementation
`Project.Types.$fEqAcceptedDecision_$c==1`. Definition:
`34bb28cc65e5cc0116298b8cff4e997221940c705803c7f42c24187fc82317b7`;
run `20260928T050138Z-2094235-exomonad-check`, log
`/tmp/recursive-nested-inherited.log`. Do not attribute this refusal to context
inheritance: that boundary had not executed.

## Generated signatures for notebook-bound handlers

The full skill recipe and the older retained-review fixture both exposed a
generated signature mentioning `Control.Monad.Freer.State.State` without an
import. Their separately bound `Handler` values used type synonyms that GHC
expanded in the generated wrapper. The pure actor example now uses ordinary declarations with adjacent signatures.
The retained-review fixture currently imports the expanded State module explicitly;
that is a workaround, not a compiler repair. The actor example passed
in both the focused notebook forms and the full 22-assertion skill recipe. This
does not establish a general repair for generated signatures of runtime-bound
handler values; that compiler limitation remains separate from memo invalidation.
