# G2 review candidate `a31c7ce0`

This is the single peer-review branch requested after the 2026-10-21 closeout
wave. It contains the first coherent H, P, T and N repair candidates and remains
explicitly unqualified.

## Source identity

| Fact | Value |
|---|---|
| Base | `90a5456584acb541c104f8da50d8c75e3aaa471d` |
| Review branch | `root/g2-final-20261021` |
| Code candidate | `a31c7ce02a98dc7954973f437d3ac407241eacd7` |
| Remote | `github`, same branch and OID |
| Tree | Clean before this documentation note |
| G2 status | Unqualified; no matched bundle or fresh Persona run |

The branch joins:

- H support composition: the initial custody repair and every-original
  follow-up.
- P exact native/package selection repair at `d5b66d2250` on its lane branch.
- T registered production histories at `7fac7d7603` on its lane branch.
- N private preparation and guarded-retirement repair at `1780e2c038` on its
  lane branch.

The dedicated lane branches remain available for review:

- `g2-next/h-support-20261021` at `1614264490`
- `g2-next/p-native-20261021` at `d5b66d2250`
- `g2-next/t-histories-20261021` at `7fac7d7603`
- `g2-next/n-private-20261021` at `1780e2c038`

## Why this shape

The original failures were not independent assertions. They came from two
boundaries:

1. Private births, current preparation, accepted deltas, checked progress,
   retirement and freeze had overlapping authority.
2. Native/package selection, completion and runtime source membership retained
   related but distinct facts through different representations.

N now gives the session a current preparation snapshot, separates accepted and
applied visible revisions, owns one authored-program identity, records
declaration and empty-program terminals, and commits retirement under one
lifecycle transition. H closes support only after exact prefix, stage,
availability and completion custody are together. P carries one exact selected
native row and source association into completion and runtime installation, and
uses one HOME policy for ordinary and item products. T registers histories that
exercise the actual source resolver and production consumers.

The branch does not add a lifecycle registry, generic workflow, shadow World,
owner-census fallback or test-only authority. It adds no `hPutStrLn`.

## Evidence that passed

- Joined production-source compile: `BUILD SUCCEEDED`, 17 local actions,
  build `0f331ad3-3e8e-4e47-86d2-853287e3d5f3`.
- Earlier joined H/P/T compile: `BUILD SUCCEEDED`, 476 local actions, build
  `bfcc6ad1-222f-4936-b427-121bbf0020d0`.
- H mixed World support: 1/1 passed, build
  `1543938e-093a-4bde-a884-75503640088d`, 16.6 seconds.
- H completion custody after role narrowing: 1/1 passed, build
  `5fe23f0a-b53e-4f92-b2d2-3e258d9425cd`.
- H shared-contribution original custody: 1/1 passed, build
  `b37fdc05-4696-464d-877b-4645edaa8e6b`.
- H production Job history crossed the former `NativeRootOutsideView`; the next
  observed frontier was `job1` host authority `None`, outside H.
- P's original nine-case package cohort: 9/9 passed, build
  `8ad4a171-32d6-4071-8243-1733cab5322b`.
- P Haskell original-root and declaration controls: 2/2 passed, build
  `997a985b-6c6e-4782-bc28-f98e1764cff8`.
- P cached-HOME item admission and affected Rust test binary compile passed.
- N's focused hidden-retirement control passed after guarded retirement.
- N integrated actor progress reached 3/6 before the final N adjustment:
  visible abandonment, host-only Text and two independently abandoned visible
  mounts passed; hidden-input, Job and checked Text exposed later frontiers.

All retained logs are under
`/srv/swarm/evidence/inanna/g2-next-wave-90a54565/`.

## Remaining red or unproven

- The expanded joined 11-case package target reached 8/11. Remaining failures
  were:
  - the equal-content/different-children fixture returned the same issuance;
  - the transitive HOME target lacked its expected target HOME row;
  - the cached HOME item test expected one row and observed two.
- The first two are under T/P premise review. The cached-HOME expectation is
  under P review. None of the old nine green cases are being discarded.
- The integrated six-case actor cohort has not yet completed on the final N+H+P
  join. N-alone Job and Text failures are expected to move after H/P but are not
  acceptance evidence.
- Ordinary activation A-to-B migration, discovery/notebook/activation/reload
  joined histories, source-publication race, disposal-safe reacquisition and the
  broader qualification gate remain open.
- Full lint is not green; the P lane retained the baseline's 33 Clippy findings.

## Attempts the reviewer should not repeat

- Do not restore the Original owner census to repair support custody.
- Do not promote `Tidepool.View.Types` out of `InterfaceOnly`.
- Do not select an inherited native row by equal payload, owner or ordinal.
- Do not recover runtime source membership by re-scanning for the product.
- Do not separate a prepared program from its target/import selection again.
- Do not treat publication while a checked item is parked as permission to
  mutate its pinned predecessor.
- Do not replace the lifecycle guard with caller-side equality checks.
- Do not run the World actor cases through direct `just test-lib`; use the
  registered owned-resident target.
- Do not merge fixture-only completion success without the next runtime source
  consumer.

## Review order

1. Review N's exact preparation, accepted/applied revision, authored-program
   progress, pending-host ownership and freeze/retirement transitions.
2. Review H's one-close support composition and exact offer/stage custody.
3. Review P's selected-row, current-source, ordinary-program and HOME policy.
4. Review T's test premises against the production producer, especially the
   equal-content issuance and transitive HOME cases.
5. Run the joined actor and 11-case package cohorts on one exact OID, then the
   ordinary activation/discovery/reload histories.

## Preserved state

The unrelated `/srv/swarm/checkouts/tidepool` checkout was not edited. Prior
dirty H instrumentation and D's retained patch remain preserved at their
recorded locations. P has a later uncommitted follow-up on its lane branch and
must not be represented by this checkpoint until committed and re-reviewed.
