# Usage/evidence integration

Tested integrated source: `c8d7058a9f9ce3bbde0be0ea7d4e73ae5f911210`.
Helper candidate `4d09c8ae803ef2d1cbb5ada6a20bc1fff3ddf7e4` was accepted after
independent review and one repair round. Recipe candidate `7567241016e3da55b3d7781b006dc7e1149446ee`
was independently accepted with review artifact `709601f0c0859ab22e1fd6aa88422130f918d6b7`.

TL directly executed in inherited repository Nix shell:

```
runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/Tests.hs
runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/RetainedSurvey.hs
python3 scripts/tests/test_lib_extract.py ExtractHelpers.test_owned_daemon_keeps_endpoint_through_worker_rotation -v
git diff --check
```

ExecutedPassed, expected passing: 14 synthetic logic cases; historical survey
assertions; one selected/executed mocked helper test, zero failures/errors/skips
(`Ran 1 test in 1.036s`, `OK`). All changed Haskell modules compiled with warnings
as errors. Diff check passed. Binaries: GHC 9.12.2 at
`/nix/store/nj7qd6d1pjy1v28bh5mniljsxfr9a57v-ghc-native-bignum-9.12.2-with-packages/bin/ghc`;
Python at `/nix/store/b5bpi6zfajzzrwwpgba2q6li3nnya4bs-python3-3.14.7/bin/python3.14`.

Review repaired a consequential evidence distinction: DirectExecution means
firsthand, not independent reviewer origin. Independence remains an explicit
separate review obligation. Historical surveys keep another actor's runs
attributed. Helpers do not authenticate artifacts or prove matrix completeness.
Task-local modules are intentionally not a new runtime API or registry.

No extractor translation, resident import, mounted service/provider, or real
Cargo/setup performance check ran. Mock timings do not justify dependency surgery
or cost superiority. Historical `/tmp` logs are local evidence references, not a
restart archive. No shipped prompt change was needed: existing shared instructions
already cover ownership, prefix capture, repairs, baseline incorporation and cleanup.
