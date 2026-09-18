# Structured swarm experiments

These synthetic worlds follow the planned-swarm, coordination and small-agent
designs (now landed; their plan documents are in Git history): authored shared
contracts, small-worker execution, consequential specialist consultation, distinct
responsibility/context/supervision graphs, and evidence attached to exact revisions.
They are not observations of a running Shoal instance.

## Expectations recorded before calls

- `world-conflict`: shared decision owned by planner; witness o1/o2; delivery and
  search affected and held for contract; independent UI continues. Old review
  does not approve new candidate; worker opinion does not erase the dependency.
- `world-local`: only the accepted contract changes, now explicitly assigning
  receiver deduplication. Owner becomes delivery; kind becomes local repair;
  delivery/search repair or revalidate; UI still continues. The large reasoner
  should not be selected for a contract that already resolves the question.
- `world-renamed`: same conflict with opaque actor IDs. Owner should become n73,
  while other decisions remain semantically unchanged. This tests ID sensitivity,
  not full field-order invariance.
- `world-packet`: 64 structured alternatives enumerate all subsets of six evidence
  pieces. Minimal sufficient packet is contract + delivery trace + consumer
  behavior (mask 7, scrambled label packet_02). Extra stale approval, unrelated UI
  progress and unsupported worker opinion are unnecessary.
- `world-packet-membership`: six necessity Nouls over the same world should favor
  contract, delivery trace, and consumer behavior, excluding the other three.
  This expectation was added after the enumerated experiment, before this call.

Each swarm response has 14 heterogeneous fields: six Choice answers, three
Score answers and five Noul answers. Dot-separated question names suggest an
authored nested record but are flat native API keys. No recursive AST generation
or implemented Haskell reconstruction is claimed. Questions cannot see each
other's answers; consistency across them is an empirical question.

The packet experiment deliberately asks Jev to solve selection over an enumerated
space. A practical implementation could use per-piece judgments plus deterministic
constraints rather than enumerate exponentially many subsets. Compare quality
and input cost before choosing that approach.

## Live results — 2026-09-16

Five requests returned HTTP 200 from `jev-1.13.0`, producing 49 answers. Each ran
once. Choice selections and qualitative branch behavior matched expectations.

| World | Decision kind | Selected owner | Delivery/search | UI |
| --- | --- | --- | --- | --- |
| Missing shared semantics | shared_decision (0.99) | planner (0.99) | hold both | continue (1.00) |
| Explicit deduplication contract | local_repair (1.00) | delivery (0.81) | repair/revalidate both | continue (1.00) |
| Missing semantics, opaque IDs | shared_decision (0.97) | n73 (0.97) | hold both | continue (1.00) |

Parenthesized values are selected-option probabilities, not workflow correctness.
All three selected witness o1/o2 (1.00). P(old review approves d19) was 0.03;
P(worker opinion rules out dependency) ranged 0.05–0.08. Local owner selection
retained 0.14 for the search worker: ownership needs more testing than decision kind.

The same implementation observations required semantic-owner attention only when
the accepted contract was incomplete; independent work continued in both worlds.
This demonstrates interpretation of authored semantics, not a universal scheduler.

### Joint selection versus per-piece judgments

The 64-alternative request selected packet_02 (contract + delivery trace + consumer
behavior), probability 0.99. It took 342 ms, using 17,821 input / 660 output tokens.
Six necessity Nouls took 221 ms, using 1,656 input / 108 output tokens:

| Evidence | P(necessary) |
| --- | --- |
| Contract | 0.69 |
| Delivery trace | 0.53 |
| Consumer behavior | 0.68 |
| Old approval | 0.23 |
| UI progress | 0.08 |
| Worker opinion | 0.14 |

A demonstrative >0.5 selection recovers the same pieces with about 10.8x fewer
input tokens. This is not a validated policy; the delivery trace barely clears
the cutoff. Joint Choice concentration and individual Noul probabilities have
different meanings and cannot be compared as equivalent confidence measures.
Independent selections need a completeness check, potentially a subsequent joint
adequacy judgment. Substitutable evidence could make independent necessity
judgments particularly fragile. The DSL needs both structured alternatives and
keyed collections, with ordinary code composing their different meanings.

### Protocol diagnostics and verification

Conflict and renamed each triggered the provisional Score mean diagnostic for UI
readiness: score 1.95 versus rounded distribution mean 1.94, and score 1.92 versus
mean 1.93. Floating-point arithmetic makes these nominal 0.01 differences exceed
the strict 0.01 check. Both calls exited 2; the other three exited 0. No retries
or overwrites occurred. The validator remains unchanged; a production acceptance
rule needs to account for independently rounded scores and probabilities.

Latencies: conflict 218 ms, local 281 ms, renamed 234 ms, joint packet 342 ms,
per-piece packet 221 ms. Private ignored evidence: `evidence/world-*-001.json`.
`src/worlds.rs` is included in harness source fingerprints. Package build, 12 unit
tests, clippy with warnings denied, formatting, and diff whitespace checks passed.
No workspace battery or running Shoal acceptance was attempted.

## Limits and next challenge

Responsibilities are explicit in this small synthetic world, sometimes also named
in reference fields. This does not establish inference of missing ownership in a
large graph. ID renaming is one metamorphic check, not statistical robustness.
No agents were woken, code integrated, recursive AST generated, or Haskell DSL
executed. The effect implementation remains pending.

Next: redundant/substitutable evidence, partially observed subtrees, and conflicting
publications at the same revision. Compare local judgments plus deterministic joins
and a final adequacy call against a whole-world decision. Preserve exact source and
observation identity in local payloads; send relevant structured descriptions.

The TypeSafe skill's state/independent-question guidance shaped these requests.
The target is internal semantic glue for small-worker swarms and occasional larger
reasoners, not conversational operator input or operator-facing intent compilation.
