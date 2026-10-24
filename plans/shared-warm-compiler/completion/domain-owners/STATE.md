# Current implementation and qualification

Updated 2026-10-23. **Ready for the user's Argon adversarial review.
G2/G3 remain unqualified.** Code candidate: `dc34094d5e`, branch
`integration/g2-domain-runtime-20261023`, checkout
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.

[ARGON-REVIEW.md](ARGON-REVIEW.md) is the current packet, including evidence,
hypotheses and test limitations. [REQUEST-SERVICE-LIFECYCLE.md](REQUEST-SERVICE-LIFECYCLE.md)
owns Option C; [NATIVE-REUSE-FRONTIER.md](NATIVE-REUSE-FRONTIER.md) records the
remaining compiler/runtime boundaries.

## Joined work

- Native authored declarations retain their exact call boundary through an
  owner-issued, memo-keyed opacity policy. The original retained-value history
  crosses its former simplification failure; actual GHC Core/interface/reuse
  coverage passes. Runtime dictionary/instance histories still fail later.
- `242bd9e0d1` retains the declaration Join's authenticated captured closure
  using actual installed compiler objects, without a hydration fallback.
- `b2602f1fec` carries stage TYPE inputs and their updated context together in
  WorldPrimaryInputs. Lexical and instance authority remain separate.
- Actual installed-interface owner controls pass after correcting their TYPE
  fixture through the production serializer (`40244afb36`).
- `f02d51dc91` removes obsolete SOURCE-capture test assumptions and observer,
  preserving native historical demand, unused custody and no-replay controls.
  `f8051efccd` fixes shared helper imports.
- `90b3e9167a` retains the originating Rust guard in ownership refusals without
  changing classification or authority. Both final runtime cases identify the
  same guard, scoped_inputs.rs:331.
- `dc34094d5e` corrects the owning borrowed-TYPE control fixture: a real sealed
  checked value enters via visible host bindings with no explicit operand.
  Its mandatory nonzero-control assertion passed in the owning libtest.

## Executed evidence

Counts describe their exact revisions, not a passing release aggregate.

| Revision | Observed result |
|---|---|
| `3e6ca9b3ef` | Five targets compiled; seven focused source-boot/parser/rooted-GHC/mixed-stage cases passed. |
| `47739467ae` | Two targets compiled; retained-plugin leaf passed. G2 crossed its original continuation and failed later. |
| `09c4aaf3de` | Eight targets compiled; 18 native/Tasty leaves: 11 service/worker passes, seven compiler/test-premise failures. Separate expected-refusal controller passed while its child remained unqualified. |
| `4c7905ff67` | Diagnostic opacity leaf failed unchanged predicate; actual Core proved the supposedly unknown seed was constant-folded. |
| `14c9b59df9` | Five targets compiled; seven executed: actual opacity Tasty passed, six runtime leaves failed later. Prior Join closure and changed-TYPE admission failures crossed. |
| `893b61862a` | Two targets compiled; two corrected retained-interface owner leaves passed. Owning World case failed its nonzero-control assertion: the operand did not exercise borrowed visible TYPE. |
| `f8051efccd` | Runtime cfg compiled; G2 and renamed historical-native case both failed at scoped_inputs.rs:331:36. Process/compiler cleanup confirmed; diagnostic completeness remained false. |
| `dc34094d5e` | Toolchain owning cfg compiled; corrected World case executed and passed with outer runner acceptance. Its visible-host/no-operand phase proved nonzero exact/omitted/changed controls. |

Exact commands, artifact hashes and reports:
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/`.
Final runtime diagnostics: `f805-checkpoint.json` and
`f805-final-owner-artifact-index.json`. Final owning-control execution:
`dc34-world-borrowed-type-owner-one-summary.json` and its per-case report.

## Remaining frontier

1. Transitive decoded thin interface has no retained actual HMI association at
   completion. Class-default and adjacent same-cell histories refuse with
   InterfaceUnownedThinHome. Investigate the projection from paired installed
   owners to transport bytes; do not weaken ownership refusal.
2. Native retained-value fallback rejects when the value is not current-produced
   **or** its exact checked association fails. Both final runtime cases identify
   that guard; current diagnostics do not distinguish its disjuncts. Challenge
   whether value age is being used in place of the demander's issued authority.
3. Re-run the joined production histories after those two repairs. Borrowed-TYPE
   refusal controls now have owning-test execution evidence; runtime tests still
   cannot execute toolchain cfg(test) controls.

Argon owns the next adversarial review. No new review swarm or broad repair wave
is being started. Existing source reviews do not establish runtime acceptance.
Main, frozen bundle and Persona remain unqualified. Further dispositions from
Argon's earlier audit remain in
`/srv/swarm/evidence/inanna/c-ownership-20261023/audits/PEER-AUDIT-REVIEW.md`;
this compiler tranche does not claim those proposals implemented.

## Ownership at checkpoint

Warm checkout `tidepool-latent-original-stage-join-20261020` belongs to
validation_resume. The final dc340 owning-test check is terminal; no further
tests are queued. Root edits the separate integration checkout. Preserve
source, WIP and retained failure evidence; never advance an active validator pin.
Latest scratch observation: about 227 GB free, with 100 GiB plus expected growth
reserved. Other implementation workers have handed off.
