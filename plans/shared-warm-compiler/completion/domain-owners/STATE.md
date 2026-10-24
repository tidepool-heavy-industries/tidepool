# Current implementation and qualification

Updated 2026-10-23. **Option C service repairs pass their regional checks.
Compiler repairs are joined at `14c9b59df9` and awaiting execution. G2 and G3
remain unqualified.**

Integration: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`,
branch `integration/g2-domain-runtime-20261023`.
[REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md) owns Option C;
[NATIVE-REUSE-FRONTIER.md](NATIVE-REUSE-FRONTIER.md) owns compiler qualification.

## Current findings and joined repairs

1. The original retained-capture failure is repaired: at `47739467ae` the actual
   G2 history reaches both observations `[23,23]`, retaining the exact Original
   use through simplification/tidy/finalization without replaying effects.
   The native-definition policy still needs its broader semantic controls.
2. The later G4 failure was not a missing usage edge in ordinary GHC output.
   Its inherited G3 is a declaration Join: its authenticated descriptor retains
   G1/G2, while the source-less GHC interface has no ordinary usage list.
   `242bd9e0d1` selects the captured closure, then pairs it with existing actual
   compiler homes. Completed issuers retain their allocations; decoded inputs
   require existing matching homes. Missing, foreign-unit, conflicting,
   wrong-NameCache and unowned thin inputs refuse. No hydration fallback.
   Independent source review accepted this repair; execution remains pending.
3. Four native-opacity histories and the adjacent SOURCE fixture all refused a
   prior-request Val.G1 before their semantic assertions. The stage borrowed the
   proper TYPE inputs but discarded their updated context. `b2602f1fec` retains
   the whole WorldPrimaryInputs bundle through admission. Lexical authority
   remains separate. Structured test observations cover exact, omitted and
   changed prior-request rows; the validator must observe nonzero coverage.
4. The new GHC opacity fixture supplied a statically known seed. Diagnostic
   `4c7905ff67` proved its consumer folded to `163 + 6*y`, invalidating the test
   premise. `a7c206b1d7` makes the input unknown; `5c711c1db3` checks the rule
   controls' actual OPAQUE metadata. Acceptance predicates remain intact.
5. `14c9b59df9` extends two existing Haskell owner leaves with retain-only
   allocation identity, absent decoded-HMI refusal, thin-input and NameCache
   refusal, and exclusion of unrelated HPT owners. It adds no fixture compiler.

## Executed evidence

Counts are revision-specific, not a passing release aggregate.

| Revision | Observed result |
|---|---|
| `3e6ca9b3ef` | Five affected targets compiled. SourceBoot 4, parser 1, rooted GHC lifecycle 1 and mixed-stage history 1 passed; both Rust cases were runner-accepted with process/compiler cleanup confirmed. |
| `47739467ae` | Two targets compiled; existing retained-plugin leaf passed. G2 failed later at the captured-interface/HMI closure after its repaired original continuation passed. |
| `09c4aaf3de` | Eight targets compiled. 18 unique native/Tasty leaves: 11 service/worker passes and 7 compiler/test-premise failures described above. Separately, the expected-refusal controller passed while its child stayed unqualified (child native assertion passed, outer runner exited 1). |
| `4c7905ff67` | Diagnostic Tasty leaf executed 1 and failed its unchanged baseline predicate, retaining the actual constant-folded Core. No additional cases were rerun. |

The service controller proves the normal runner refuses retained cleanup; its
success does not qualify its child. Diagnostic completeness remains independent
of process/compiler cleanup and is not inferred from it.

Earlier exact commands, counts and reports remain in
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/`.
The current completed checkpoints are `09c4-checkpoint.json` and
`4c79-checkpoint.json`. Source review and root-cause evidence are under
`/srv/swarm/evidence/inanna/c-ownership-20261023/audits/`.

## Remaining work and owners

- `validation_resume`: compile the joined worker/runtime consumers; execute G2,
  the opacity Tasty leaf, four real opacity histories, the adjacent historical
  SOURCE fixture, and the two retain-only owner leaves. Do not rerun unchanged
  service controls. Preserve actual coverage, failures and cleanup distinctions.
- Root: inspect all failures, qualify the closure/context repairs, review joined
  semantics and remove obsolete paths rather than introducing alternate routes.
- `audit_haskell_dead_work`: own any further Core/interface/fixture findings.
- `w2_request_protocol`: own stage TYPE propagation and runtime histories.
  Its separate SOURCE-fixture investigation established that INLINE on a native
  declaration does not issue SOURCE authority. Ordinary closed source publication
  also refuses retained captures. Resolve that stale premise after the earlier
  admission blocker clears, preserving native historical/dead-use controls and
  genuine SOURCE protocol/owner tests.
- Astra reviewers: independently inspect repaired owners and production-path
  controls. Bounded source acceptance is recorded; execution is separate.

After these checks: adversarial joined/purge review, then the maintained G2
cohort. G3, bundle freezing and fresh Persona follow actual G2 acceptance.
Argon's further dead-code/equality proposals remain dispositions in
`/srv/swarm/evidence/inanna/c-ownership-20261023/audits/PEER-AUDIT-REVIEW.md`;
this compiler repair does not silently claim them implemented.

## Build ownership

The warm checkout `tidepool-latent-original-stage-join-20261020` belongs to
`validation_resume`. Advance it only after its explicit pin release. Root edits
its separate integration checkout. Latest scratch observation is about 228 GB
free; preserve 100 GiB plus expected growth. Main, bundle and Persona are not
qualified. Source, WIP, retained failures and compiler cleanup evidence survive.
