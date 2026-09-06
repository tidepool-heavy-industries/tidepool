# Root usage integration check

Accepted lead candidate: 02ee6158c46088aa8d081a50de8afb7bccf1210d.
Integrated and directly tested root revision: 5a7b21ae5b7f0d03b024f170bba54dcf774addf1.
Root reviewed helper implementations, usage instructions and lead evidence before
merging. In the inherited repository Nix shell (`IN_NIX_SHELL=impure`), root ran:

- `git diff HEAD^ --check`: ExecutedPassed, expected passing.
- `runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/Tests.hs`:
  ExecutedPassed; printed `14 synthetic evidence logic checks passed`.
- `runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/RetainedSurvey.hs`:
  ExecutedPassed; historical coordinator view was Insufficient for two attributed
  checks; implementer firsthand view was PassingEvidence, explicitly not independent.

Resolved runghc:
`/nix/store/nj7qd6d1pjy1v28bh5mniljsxfr9a57v-ghc-native-bignum-9.12.2-with-packages/bin/runghc`.
These checks compile all delivered Haskell modules with warnings as errors. Root
has not rerun the unchanged mocked Python test; its success remains lead-attributed.
No resident import, extractor translation, mounted service or performance claim
is established by these portable helper tests. Full command output is retained in
root's native-tool conversation receipt.

The exact accepted baseline was sent to the retained usage lead in a new scoped
UX follow-up; incorporation and resulting verification remain pending its reply.
