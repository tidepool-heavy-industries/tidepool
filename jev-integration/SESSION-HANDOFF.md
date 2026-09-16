# Jev research session handoff

Recorded 2026-09-16 before conversation compaction.

## Checkout and scope

- Isolated worktree: `/home/inanna/dev/tidepool-jev-integration`
- Branch: `research/jev-integration`
- Starting revision: `00f8f3332`
- Revision containing this handoff: see `git log -1` after commit
- The main `/home/inanna/dev/tidepool` checkout and the unrelated Core → STG
  cutover were deliberately left alone.
- `jev-integration` is a member of Tidepool's Cargo workspace but has no engine
  dependencies. All focused compilation and testing used `-p jev-integration`.
- No production Jev effect, protocol verb, handler, credential loader, actor
  integration, or engine change has been implemented. This branch is a research
  harness, Haskell feasibility sketch, evidence, and design handoff.

## User direction that must survive compaction

The goal is one Haskell effect operation matching the full native Jev request,
not separate `choose`, `score`, or `noul` effect verbs and not a simplified
router. The model-facing DSL should use Servant-style mode-interpreted records,
admit every valid provider shape, reject invalid shapes where Haskell can do so,
support nested/static and runtime-sized question collections, and retain typed
local payloads behind Choice alternatives.

Jev is internal semantic glue for trusted Shoal codebase state. It is not an
operator chatbot, not a generated-command mechanism, and not primarily a batch
system. Bounding boxes/images are explicitly out of the current scope. The user
corrected an attempted prompt-injection experiment: security/adversarial text is
not the frontier to optimize here. Optimize useful intelligence per resident
turn instead.

The intended mature role is a **typed semantic control plane** or **semantic
branch predictor between deterministic phases**:

1. Haskell runs Bash/LSP/git/actor queries and computes exact relationships.
2. It constructs a bounded semantic frontier over one trusted world-state.
3. One Jev request returns roughly 5–20 useful related judgments in normal use.
4. Typed Haskell policy selects retained commands, edges, actors, evidence, and
   continuations and performs the authorized actions.
5. Another Jev call occurs only after new evidence or a genuinely new semantic
   boundary, not after every mechanical step.

Exact traversal, joins, counters, ancestry, cycle handling, source consistency,
authorization, scheduling, and execution remain deterministic. Jev supplies
semantic selection, relevance, sufficiency, contradiction, urgency, and
escalation judgments.

## Committed artifacts

Primary entry points:

- `jev-integration/README.md` — how to build and use the research crate.
- `plans/jev-dsl.md` — full DSL/API research handoff and unresolved decisions.
- `plans/jev-core-shoal-uses.md` — seven paired Haskell DSL and Jev wire-call
  sketches for the mature semantic-control-plane role.
- `jev-integration/CONTRACT.md` — observed request/response contract matrix.
- `jev-integration/FRONTIER-EXPERIMENTS.md` — measured capability frontier.
- `jev-integration/BASH-LSP-EXPERIMENTS.md` — ten paired Bash/LSP decision chains.
- `jev-integration/NOTEBOOK-SIMULATIONS.md` — bounded dependent simulations.
- `jev-integration/SHOAL-EXPERIMENTS.md` and `WORLD-EXPERIMENTS.md` — routing,
  traversal, evidence, repair, and structured agent-world experiments.
- `jev-integration/VALIDATION.md` — earlier scaffold verification record.
- `jev-integration/src/frontier.rs` — reproducible live frontier case catalog.
- `jev-integration/haskell/` — positive and compile-fail DSL feasibility sketches.

The publisher's TypeSafe skill is vendored exactly, including its MIT license:

- `jev-integration/vendor/typesafe-ai/SKILL.md`
- `jev-integration/vendor/typesafe-ai/LICENSE`
- `jev-integration/vendor/typesafe-ai/UPSTREAM.md`

It is pinned to upstream commit
`65a39f393687675ce170e6094757de20370365b9`. Local and upstream SHA-256 hashes
were compared and matched for both `SKILL.md` and `LICENSE`. The skill is an
agent/research guide, not runtime code. It says live TypeSafe documentation is
the source of truth.

## Raw evidence custody

`jev-integration/evidence/` currently contains 316 JSON files totaling 8,592,562
bytes. Every checked evidence file has Unix mode 0600. The captures include exact
request JSON, response bytes/JSON, elapsed time, usage, model, harness fingerprint,
and provisional typed interpretation. The harness performs one attempt, does not
retry or follow redirects, bounds response capture, and redacts credential echoes.

**The entire evidence directory is git-ignored. Raw exchanges are not in the
commits and will be lost if this worktree/evidence directory is deleted.** The
committed Markdown records conclusions, representative numbers, caveats, and
reproduction commands. Preserve the worktree if raw evidence will be needed.

No API key is copied into Git or this handoff. Live calls received
`TYPESAFE_API_KEY` through the process environment. Do not print or commit it.

Evidence families present locally:

- Initial contract probes at `jev-integration/evidence/*.json`, including state,
  instruction, Choice, Score, Noul, cardinality, unknown-field, model, structured,
  OpenAPI, Shoal, and world cases.
- Four initial notebook simulations: `notebook-{failure,source,context,swarm}-001`.
- Thirteen Bash/LSP runs covering trace, verification, implementation reuse,
  migration archaeology, and reproducer selection, including absence controls.
- Full frontier runs `frontier-001` through `frontier-005`.
- Focused frontier runs `frontier-006-*`, `frontier-007-*`, and
  `frontier-008-fanout1024`.

## Provider and contract findings

- Requested alias `jev-latest` resolved to observed model `jev-1.13.0` during
  these experiments.
- The request spine is model + non-null structured state + nonempty question map.
  One invalid question rejects the whole request.
- Choice, Noul, and Score accept rich structured instructions/criteria with the
  position-specific distinctions recorded in `CONTRACT.md` and `jev-dsl.md`.
- Tested Score cardinality is 1–10; tested Choice cardinality succeeds through
  255 and rejects 256. Question maps succeeded far beyond 256 until request/token
  limits were reached.
- Choice always ranks supplied alternatives. Supply a real no-match alternative
  when appropriate and use a separate Noul when presence/coverage is independently
  useful.
- Questions in one request are evaluated independently over the same state and
  cannot consume sibling answers. Speculative questions must state their premise.
- Question IDs are code association keys and are not model-facing. Choice keys
  and descriptions are model-facing and materially affect inference.
- An undocumented/account-disabled `bounding_box` discriminator was observed but
  not reverse-engineered or added to the proposed text/glue DSL.
- Responses occasionally contained rounded Choice distributions whose sum differed
  from one by more than the provisional 0.01 tolerance while the winning key was
  correct. Response-contract validity and semantic selection are separate gates.

## Capability frontier measured

### Strong region

- Correct six-field conjunctive selection among 8, 32, 128, and 255 near-neighbor
  alternatives, including four relabelings.
- Correct explicit no-valid-option judgments through 128 candidates, subject to
  intermittent probability-sum formatting findings.
- Correct semantic graph-path selection through seven edges and correct no-path.
- Correct temporal accepted/superseded/draft/off-branch/exception decisions.
- Six mutually checking Noul judgments all landed on the expected side.
- A realistic Shoal microprogram answered three Choices plus four Nouls in one
  request: mechanism, next LSP action, verification target, wake policy, evidence
  support, contract completeness, and semantic-decision need. It passed five
  observed attempts at roughly 135–208 ms, using 931 input and 251 output tokens.
- Speculative exact-judgment fan-out passed every answer through 640 questions.
  The 640-question request completed in 586 ms with aggregate usage of 43,784
  input and 14,724 output tokens. This is a capacity result, not evidence that
  640 simultaneously hard semantic questions would all be reliable or desirable.
- One focused Choice remained correct with probability 1.0 through 768 irrelevant
  structured records and 32,568 input tokens.

### Failure region

- Exact shuffled pointer traversal passed depths 1, 2, and 4, was unstable at
  depth 8 (one pass, two failures observed), and failed every observed attempt at
  depths 16, 32, and 64. Jev is not a deterministic graph interpreter.
- Single-question states with 896, 960, and 1,024 synthetic irrelevant records
  returned HTTP 400 `max_tokens_exceeded`; 768 records passed.
- 1,024 parallel synthetic questions returned the same error; 640 passed.
- Equivalent Choice descriptions did not receive equivalent probabilities when
  their option keys differed. Observed splits included 0.88/0.12 for
  `route_a`/`route_b`, 0.90/0.10 for `first`/`second`, 0.06/0.94 for
  `option_z`/`option_a`, and 0.66/0.34 for opaque `k019`/`k873`. Coalesce
  equivalent continuations in Haskell and give retained alternatives intentional
  semantic names. Do not interpret a duplicate-option split as fair calibration.

### Architectural conclusion

The frontier is **semantic width, not exact depth**. Give Jev many nuanced
alternatives, independent judgments, structured exceptions, and rich local
evidence. Keep repeated state transitions and exact computation in Haskell.
Wide question-rich packets are valuable because they can precompute branch-local
semantic facts over already available evidence; extreme fan-out was a boundary
probe, not the intended everyday API style.

## Mature high-leverage uses recorded

`plans/jev-core-shoal-uses.md` contains complete paired sketches for:

1. Investigation turn compilation.
2. Semantic steering of deterministic code-graph traversal.
3. Multi-recipient swarm routing and wake-versus-queue annotation.
4. Evidence relevance, support, contradiction, and packet construction.
5. Adaptive focused-to-broad verification planning.
6. Explicit semantic stop/continue/consult/escalate conditions.
7. Cheap supervision between expensive Astra/Fable-level awakenings.

All seven embedded JSON request bodies were parsed successfully. They are design
mockups, not live results. Commands, edges, evidence references, actor handles,
and continuations stay as local typed Haskell payloads; only their descriptions
go to Jev.

## Commits made in this worktree

- `f3559048a` — isolated Jev API research crate and DSL sketches.
- `b351df6c2` — contract boundary map.
- `8eb567a2a` — Shoal decision-chain experiments.
- `80510d477` — initial decision-frontier probes.
- `d107e25e2` — efficacy frontier and vendored TypeSafe skill.
- `d72b02147` — semantic-control-plane DSL/wire examples.
- A later commit contains this session handoff.

## Verification completed

At the last code-changing checkpoint:

- `bash scripts/dev-shell.sh cargo test -p jev-integration -- --test-threads=1`
  passed 15 tests.
- `bash scripts/dev-shell.sh cargo clippy -p jev-integration --all-targets -- -D warnings`
  passed.
- `git diff --check` passed.
- The seven JSON blocks in `plans/jev-core-shoal-uses.md` were parsed with a JSON
  parser.
- The worktree was clean immediately before adding this handoff.

The recurring Nix warnings about an untrusted Cachix substituter were environmental
and did not fail these focused checks.

## What remains deliberately unproven

- No representative repository-derived evaluation set has measured accuracy,
  calibration, cost, or savings against actual Shoal frontier turns.
- The 640-question test used exact presence judgments to measure fan-out capacity;
  it is not a semantic-quality benchmark.
- Five successes for the seven-answer synthetic microprogram are promising but
  not a production reliability estimate.
- No concurrency, cancellation, retry, rate-limit, credential-authority, or
  resident-session integration policy has been settled.
- The proposed generic codec, effect schema, Rust handler, and Haskell response
  association types do not exist yet.
- Live docs were read from their moving official URLs and are linked/datestamped
  in the design documents; aside from the pinned skill and OpenAPI fixture, they
  were not vendored as a documentation mirror.

## Best next work after compaction

Do not keep increasing artificial question counts. Build a small labeled corpus
from real or realistic Shoal investigation snapshots and compare:

1. deterministic baseline only;
2. one Jev decision packet;
3. ordinary multi-turn frontier-model investigation.

Measure correct continuation, evidence recall, unnecessary tool calls, frontier
turns avoided, latency, token/cost usage, low-margin/escalation behavior, and
stability under meaningful relabeling. Use 5–20 coherent questions per packet.

Then refine the Haskell sketch around the proven high-leverage record shapes:
dynamic `Each` questions, heterogeneous retained alternatives, explicit no-match,
response association, intentional Choice labels, and pure policy combinators over
probabilities. Production integration should wait for the unrelated engine work
and must begin by rereading the nearest `AGENTS.md` and owning sources.

## Compact resume statement

Resume in `/home/inanna/dev/tidepool-jev-integration` on
`research/jev-integration`. Read this file, `plans/jev-dsl.md`,
`jev-integration/FRONTIER-EXPERIMENTS.md`, and
`plans/jev-core-shoal-uses.md`. Preserve `jev-integration/evidence/` if raw live
exchanges matter. Continue efficacy evaluation with realistic Shoal snapshots;
do not modify the in-flight engine or mistake extreme fan-out for the product.
