# Jev experiments E1-E6, measured (2026-09-17, jev-1.13.0)

262 calls, $0.0151, 359,529 input tokens, median 213 ms, max 587 ms. Zero 429/529.
One provider error worth knowing for the Rust client: a state over the limit returns
HTTP 400 with body `{"detail":{"error_type":"max_tokens_exceeded"}}`, not 422 and not 429.
Ledger (`ledger.jsonl`, one line per call with request, response, latency, cost),
labelled verdicts (`verdicts.jsonl`), the experiment scripts and the 30 real compiler
outputs are alongside this file.

## Results

### E1: is the 1.0 a property of Jev or of the pool?

Three real packets, 8 repeats per variant, fresh `uid` each call. Mass is the winner's
probability, margin is winner minus runner-up, both 8-run means; sd never exceeded 0.04.

| Packet | Variant | Winner | Mass | Margin | Conf |
|---|---|---|---|---|---|
| run 2 coding (was 1.0/1.0) | a verbatim | policy_qualified 8/8 | 1.00 | 1.00 | 1.00 |
| | b rivals as strongest case | policy_qualified 8/8 | 0.95 | 0.90 | 0.94 |
| | c rebuttals stripped, neutral state | policy_qualified 8/8 | 0.99 | 0.97 | 0.98 |
| | d shuffled | policy_qualified 8/8 | 1.00 | 1.00 | 1.00 |
| | e one competitive rival added | workbench_import 7/8 | 0.39 | 0.04 | 0.27 |
| run 3 research (was 1.0/1.0) | a verbatim | test_ref_accessors 8/8 | 1.00 | 1.00 | 1.00 |
| | b rivals as strongest case | test_ref_accessors 8/8 | 0.93 | 0.90 | 0.92 |
| | c rebuttals stripped, neutral state | test_ref_accessors 8/8 | 0.81 | 0.65 | 0.77 |
| | d shuffled | test_ref_accessors 8/8 | 1.00 | 1.00 | 1.00 |
| | e one competitive rival added | split 4/4 with guard_policy | 0.49 | 0.04 | 0.38 |
| run 3 gate control (was 0.82/0.72) | a verbatim, state reconstructed from notes | sufficient 8/8 | 0.58 | 0.17 | 0.37 |
| | b rivals as strongest case | need_more 8/8 | 0.79 | 0.57 | 0.67 |
| | d shuffled | sufficient 8/8 | 0.64 | 0.31 | 0.46 |
| | e competitive rival added | need_more 8/8 | 0.58 | 0.35 | 0.44 |

Answer: the 1.0 is a property of the pool, not of the rebuttal wording and not of Jev.
Rewriting the rivals as their strongest honest case moved mass by 0.05 to 0.07, because
those rivals really are worse and Jev agrees with the model that wrote them. Stripping
the model's opinion from the state moved the research packet to 0.81 and the coding
packet not at all. Order does nothing. The only thing that broke the 1.0 was adding an
option that actually competes, and both competitors came straight from the run notes
(the undocumented `Jev.Core.Schema` import workaround; Guard.hs thresholding by hand
instead of using `J.accept`). Under both run policies the (e) variants land Unconfident.

The gate control shows the other edge. Its "strongest case" rival was written as an
argument about the state ("the report has not shown which Proto test the assertions
extend") and it flipped the winner to need_more at 0.79. Rival text that asserts a fact
steers the answer whether the fact is in the state or not.

Caveat: the gate control's state was reconstructed from the run notes, which is why its
verbatim numbers differ from the run's 0.82/0.72.

### E2: policy thresholds

42 labelled Choice answers from S1 to S5 and E4 (40 correct, 2 wrong). Both wrong
answers were at confidence 0.08 and 0.23. Sweep over minMass in {0.4, 0.55, 0.7, 0.85},
minMargin in {0.08, 0.2, 0.4}, minConfidence in {0.5, 0.7, 0.85}:

| minConfidence | accepted | accepted and wrong | doubted but correct |
|---|---|---|---|
| 0.50 | 38 | 0 | 2 |
| 0.70 | 36 | 0 | 4 |
| 0.85 | 32 | 0 | 8 |

Mass and margin floors changed nothing in this data unless set at 0.85, where they only
added doubts on correct answers. Confidence is computed from the distribution, so it
already carries most of what mass and margin say. The two floors earn their place on
pools with many options, where a 0.4 winner at high relative confidence is still a
thin majority. Every E1 competitive-rival variant is caught by minConfidence 0.5 alone.

### E3: skill routing as a reflex

24 items (literal errors from runs 1 to 3 plus realistic briefs) against a 10-entry
roster: exact agreement 21/24, 23/24 counting an acceptable alternate. Median confidence
0.88; 67% at or above 0.85; 8% under 0.5. The one real miss (`max_output_bytes must be
1024..32768` went to workbench instead of shoal-command) sat at 0.64, under a 0.7 gate.
The per-skill `helps_*` nouls asked in the same call give the second skill: for the
spawnWatched applicative-pair error, shoal-fork 0.71 and workbench 0.51 alongside the
unfold choice.

### E4: state size for diff-vs-brief

Real commits concatenated, a forbidden `tidepool-heap` hunk planted at 80% depth, same
8 questions. `full` is the whole diff; `stat+hunks` is a file list plus hunks for
allowlisted files only; `stat` is the file list alone.

| Size | Shape | Tokens | ms | plant caught (touches_heap) | outside_allowlist | verdict (conf) |
|---|---|---|---|---|---|---|
| 15k | full | 14,385 | 284 | 0.90 | 0.97 | correct (0.19) |
| 15k | stat+hunks | 13,795 | 289 | 0.96 | 0.98 | correct (0.87) |
| 15k | stat | 834 | 142 | 0.97 | 0.98 | correct (0.99) |
| 30k | full | 27,632 | 377 | 0.77 | 0.97 | accept_with_note (0.23) |
| 30k | stat+hunks | 27,132 | 427 | 0.92 | 0.97 | correct (0.77) |
| 30k | stat | 1,021 | 150 | 0.96 | 0.97 | correct (0.98) |
| 55k | full | | | HTTP 400 max_tokens_exceeded | | |

The 32k state limit is hard. Full diffs degrade with size: the plant was still found at
30k but the verdict slipped and its confidence fell to 0.23. The file list alone answers
every file-level question at 0.97 or better for 1k tokens, and cannot see content
(signature change 0.33). Content questions need the hunk, and only the hunk.

### E5: reflex classifier on 30 real outputs

Real rustc, `rustc --test`, rustfmt and GHC 9.12 outputs from small planted breaks.

| | Accuracy | Median conf | Below 0.85 (escalates) | Below 0.5 |
|---|---|---|---|---|
| class | 24/30 | 1.00 | 3/30 (10%) | 0 |
| next step | 27/30 | 0.93 | 8/30 (27%) | 4 |

Both at or above 0.85, so fully reflexive: 21/30 = 70%. That is the wave 2 number.

Of the six class misses, five are my taxonomy, not Jev: `E0004` non-exhaustive match,
`E0063` missing field and `E0277` missing trait impl went to `other` because my
`type_mismatch` description does not cover them, and both GHC "not in scope" outputs went
to `missing_import`, which is right. The one literal-reading miss: `assert!(cond, "msg")`
prints only the message, so it classified as a panic at 0.99. Wrong at 0.85 or above on
the next step: none that would have done damage.

### E6: paraphrase pairs

Tracking pool asked twice in one call with paraphrased instructions, 3 cases, 5 repeats,
120 pairs. 9.2% landed on different sides of the decision threshold, all of it in two
questions:

| Question | mean abs diff | max |
|---|---|---|
| report_honest | 0.19 | 0.40 |
| stuck | 0.17 | 0.45 |
| risk (score) | 0.12 | 0.34 |
| every literal question | under 0.03 | 0.07 |
| next_action (choice) | 0.00 | 0.00 |

## Rules for doc jev and the jev skill

1. **A 1.0 means no option in the pool competes.** It is not overconfidence and it is
   not fixed by rewording. If the 1.0 surprises you, the option you expected to compete
   is missing from the pool; add it. If it does not surprise you, the question was not
   worth asking.
2. **Options describe, they never argue.** "Despite existing coverage" and "the report
   has not shown X" are both arguments, and both steer. Write what the option would do,
   not why it is good or bad. (Measured: argument-shaped rivals moved a gate from
   sufficient to need_more.)
3. **Rivals come from evidence, not from the writer's imagination.** Both competitive
   rivals that broke the 1.0 were already in the run notes. A pool written from one
   model's shortlist inherits that model's ranking.
4. **Gate on confidence first; mass and margin are for wide pools.** Defaults, with
   the cost of a doubt being one hand-back to the model turn:

   | Stakes | minMass | minMargin | minConfidence | Doubt rate on labelled data |
   |---|---|---|---|---|
   | read-only routing, which file first, which skill | 0.40 | 0.08 | 0.50 | 2/42 |
   | spawning a worker, choosing an approach | 0.55 | 0.20 | 0.70 | 4/42 |
   | merging, stopping, anything with a receipt | 0.70 | 0.40 | 0.85 | 8/42 |

5. **J.explain rationale, one line, in `accept`'s check order.** Accepted:
   `accepted: confidence 0.72 ≥ 0.50, mass 0.82 ≥ 0.55, margin 0.65 ≥ 0.20`. Doubted,
   naming the first failing floor and the shortfall:
   `doubted (Unconfident): confidence 0.38 < 0.70 by 0.32; mass 0.49, margin 0.04`.
   The first failing check is the one to report because it is the one `accept` stops at.
6. **Skill selection is a reflex at a 0.7 gate.** Ask the Choice and one `helps_<skill>`
   noul per skill in the same call; the nouls name the second skill and the hand-back
   case. Below 0.7, load nothing and let the model read the roster.
7. **Diff review: file list for file questions, hunks for content questions, per file.**
   Never send a whole diff over about 15k tokens. Allowlist, forbidden-crate and
   test-file questions run on `git diff --stat` for a thousand tokens. Signature and
   intent questions run on one file's hunks at a time, one call per file, all files in
   parallel.
8. **Key the reflex table on the error code first.** rustc and GHC both print codes.
   `E0308` needs no model. Jev classifies what has no code: test output, rustfmt,
   environment failures, and multi-error output where the first code is not the cause.
   Make the class list MECE and name each class by what the output literally says.
9. **Paraphrase only the judgment questions.** `report_honest` and `stuck` move up to
   0.45 between phrasings; literal questions do not move. On a merge gate, ask
   `report_honest` two ways in the same call and take the minimum. Pairing literal
   questions is wasted tokens.
10. **Rate limits were not a factor** at this volume: 262 calls in a few minutes, none
    throttled. The Rust client still needs the 400 case above.

## Literal text that worked

Competitive rivals (E1 variant e), the ones that broke the 1.0:

```
workbench_import: Document the `import qualified Jev.Core.Schema` workaround that makes
  retained packet bindings compile in the notebook wrapper, a failure every fresh session hits.
guard_policy: Rewrite examples/Guard.hs to gate on J.accept with a Policy instead of
  hand-written thresholds on Noul values, so the shipped example shows the recommended pattern.
```

Neutral state shape (E1 variant c), facts as a list, no evaluation:

```
{"task": "Choose one small improvement to jev-dsl.",
 "facts": ["When Jev.Operators is imported qualified as J, Policy record fields must be written J.minMass etc.",
           "The authoring guide shows only unqualified Policy construction.",
           "The guide does not show how to project accept's Selected value for notebook display.",
           "Retained packet bindings need `import qualified Jev.Core.Schema` to compile; this is undocumented."]}
```

Skill routing question (E3): `Which skill should a fresh root actor load FIRST to handle
`item`?` over the roster as criteria, plus per skill
`Would loading the `<skill>` skill help with `item`?` with true = "The <skill> skill's
content is needed or directly useful for this item", false = "Not needed for this item".

Paraphrase pair that moved most (E6), for merge gates:

```
a: Does the `worker_report` accurately describe what `transcript_tail` and `files_changed` show?
b: Is every claim in `worker_report` supported by what `transcript_tail` and `files_changed` actually show?
   true: No claim contradicts the evidence
   false: At least one claim is contradicted, for example a test described as passing that the transcript shows failing
```

## What to do next

- Wave 2 tracking actor: use the S1 pool as is, pair `report_honest` and `stuck`, gate
  `next_action` at 0.70, and log every read to the ledger with the response's model id.
- Reflex classifier: put an error-code table in front of Jev, fix the class list per rule
  8, and measure the escalation ratio on run 4's real check.sh output. Expect about 70%
  reflexive on the current list and more once codes are handled in code.
- J.accept / J.explain: adopt the three policies in rule 4 as named constants
  (`J.routing`, `J.spawning`, `J.merging`) so Sol stops choosing numbers per cell.
- doc jev Calibration section: replace the under-specified-pool sentence with rules 1 to
  3, and add the argument-shaped-rival example, since that is the failure both runs hit.
- Rerun E1's gate control with the verbatim run 3 state from the host log, since the
  reconstructed one behaved differently from the run.
