# Learn the published TypeSafe patterns first

Research and experiment plan, 2026-09-17. Companion to
[tool improvement](jev-tool-improvement.md),
[shared System 1 components](jev-shared-components.md), and
[swarm leverage](jev-swarm-leverage.md).

## Direction

We built a reconfigurable Haskell interaction surface so we can reshape our
programs around what the models actually do well. Start from TypeSafe's published
patterns, especially their advanced compositions. Adapt the playground and our
habits to those capabilities. Do not force Jev into a familiar orchestrator or
reduce it to approval gates because those were our first experiments.

“Use every pattern to the hilt” means understand and try its useful consequences,
including combinations. It does not mean insert every pattern into every run.
The goal is more useful computation per expensive model turn, with less irrelevant
context and room for agents to invent and share better programs.

The next one or two dogfood runs remain one Sol with Luna subagent trees on a
medium-sized task. This is a good setting for rich authored cells and small
controllers; autonomous multi-Sol coordination is not a prerequisite.

## Research status

Fetched 35 live documentation pages on 2026-09-17, including 18 cookbooks.
The source index below is durable; the local research snapshot is temporary:
`/tmp/typesafe-patterns-2026-09-17/`, with URLs and headings in `inventory.json`.

Reading depth differs. The most detailed second pass covered function calling,
structure recovery, autoresearch, hierarchical search, entity alignment, date
extraction, and the smart-home example. Some recipes were reviewed at the prose
or selected-code level; this is not an audit of every implementation or an
exhaustive inventory of every public TypeSafe example. No authenticated calls or
local reproductions were run for this research. Published outcomes remain vendor
examples, not measurements of Shoal.

## Patterns that change our priorities

### 1. LLM-authored semantic features, improved from failures

The [autoresearch cookbook](https://docs.typesafe.ai/cookbooks/autoresearch_feature_discovery.md)
uses an LLM to propose questions, Jev to turn text into numerical features, and a
supervised model to use those features. Model errors and feature importance feed
the next proposal. Its example finds most improvement in the initial question
set, with additional gains from iteration.

**Our adaptation:** a Sol or Fable authors a bundle of semantic questions for a
recurring task. Save the observations, judgments, actual outcomes, and a few
failures. Revise the questions and composition from those failures; share the
result as Haskell source. This is a concrete precedent for our System 2 agents
improving System 1 components.

A first trial needs a useful component and several revealing examples, not a
CatBoost service or a large benchmark. If we later have enough labeled outcomes,
the full feature-learning pattern is a separate attractive option. Inspect which
questions help decisions rather than rewarding sheer question count.

### 2. Recover structure and transform content without generating it

The [formatting cookbook](https://docs.typesafe.ai/cookbooks/autoformat.md) first
judges neighboring line boundaries, then classifies reconstructed blocks and
asks speculative questions about their possible roles. Code assembles the
output. The example normalizes whitespace and adds markup; it is not a promise
of byte-for-byte preservation.

**Our adaptation:** tool output becomes navigable evidence: source excerpts,
commands, diagnostics, repetition, warnings, and prose. Code retains original
bytes and offsets; semantic judgments supply grouping, ordering, and optional
elision. This is richer than asking which twenty lines to hide. It can also make
child reports usable without asking an LLM to rewrite them.

Try an ordinary noisy output from a real task. Can one cell give Sol a useful
view while every omitted block remains retrievable through existing pagination?
Preserve contradictions and uncertainty as explicit material to inspect.

### 3. Select typed actions and arguments in one semantic pass

The [function-calling cookbook](https://docs.typesafe.ai/cookbooks/function_calling.md)
reads closed argument sets from signatures and attaches semantic descriptions.
One packet asks about the function and possible arguments across branches; code
uses only the selected branch. Presence questions distinguish omission from an
explicit setting. Free-form arguments are not magically supplied by this method.

**Our adaptation:** an authored controller offers concrete Haskell actions with
known inputs: inspect these source ranges, compare these candidates, fetch this
evidence, run this existing check, ask this worker. Ask branch-specific questions
speculatively, then execute the selected typed action. Described no-match and
missing-evidence outcomes belong in the menu when applicable.

This is a stronger starting point for Jev-piloted work than classifying an
unstructured transcript into a generic “next action.” The available actions,
observations, and their consequences are part of the program.

### 4. Keep several plausible paths alive

The [hierarchical-classification cookbook](https://docs.typesafe.ai/cookbooks/hierarchical_classification.md)
explores multiple paths instead of committing at each level to the local winner.
It uses geometric-mean path scores. The inspected implementation parallelizes
requests with a thread pool; do not describe that implementation as one batched
API request per depth. Its path scores are ranking heuristics, not calibrated
probabilities that a final diagnosis is correct.

**Our adaptation:** investigate several candidate subsystems, symbols, or
explanations cheaply before asking an LLM to expand one. Source structure and
ordinary search provide candidates; Jev selects worthwhile branches; code fetches
new observations. A bounded investigation can finish with evidence or a few
specific unresolved branches for Luna.

First try this on source discovery with known structure. Extending it into repair
search is an experiment, not something the classification example proves.

### 5. Make judgments reusable data

[Composite scoring](https://docs.typesafe.ai/patterns/composite-scoring.md) separates
semantic measurements from how code combines them. Ask several dimensions once,
then vary policy without another model call.

**Our adaptation:** a candidate can carry relevance, evidence support, novelty,
contradiction, and likely usefulness to several consumers. The debugger,
reviewer, and context selector use the same observations differently. Keep
noncompensating conditions separate: averaging must not erase a decisive failure.

Changed evidence or changed question meaning requires fresh judgment. Reuse is
not permission to apply a previous answer to a repaired candidate.

## Full cookbook map

Each proposed application below is our inference from the linked example.

| Published cookbook | Mechanism to recover | Haskell/Shoal experiment |
| --- | --- | --- |
| [Parallel questions](https://docs.typesafe.ai/cookbooks/parallel_questions.md) | Multiple useful answers about the same state together | One substantial observation bundle consumed by several branches |
| [Function calling](https://docs.typesafe.ai/cookbooks/function_calling.md) | Closed action and argument selection, optional argument presence | A controller with typed action payloads |
| [Autoformat](https://docs.typesafe.ai/cookbooks/autoformat.md) | Boundary judgments, block roles, speculative companions, code rendering | Lossless-source tool views and structured reports |
| [Autoresearch](https://docs.typesafe.ai/cookbooks/autoresearch_feature_discovery.md) | Proposed semantic features revised using downstream errors | Shared question bundles improved between dogfood runs |
| [Hierarchical classification](https://docs.typesafe.ai/cookbooks/hierarchical_classification.md) | Multiple candidate paths through a hierarchy | Source investigation that retains competing leads |
| [Entity alignment](https://docs.typesafe.ai/cookbooks/entity_alignment.md) | Pairwise semantic identity plus explanatory field judgments | Relate duplicate findings or similar saved components |
| [Semantic find](https://docs.typesafe.ai/cookbooks/semantic_find.md) | Meaning-based selection of identified source material | Find the relevant region in unfamiliar output |
| [Reranking](https://docs.typesafe.ai/cookbooks/rerank_typesafe.md) | Judge retrieved candidates against the actual question | Rank source reads or useful evidence before spending a Sol turn |
| [RAG passages](https://docs.typesafe.ai/cookbooks/classifying_rag_passages.md) | Distinguish relevance, usable evidence, contradiction, instruction attempts | Prepare context while preserving conflicting evidence |
| [Citation checking](https://docs.typesafe.ai/cookbooks/citation_check.md) | Exact source checks combined with semantic support judgments | Check a report's claims against source excerpts and actual test bodies |
| [Skill suggestion](https://docs.typesafe.ai/cookbooks/skill_suggestion.md) | Broad shortlist followed by detailed selection with rejection | Discover saved System 1 functions and relevant skills |
| [Pre-parsed extraction](https://docs.typesafe.ai/cookbooks/pre_parsed_value_extraction_cookbook.md) | Code finds candidate values; semantics assigns roles; code copies values | Recover OIDs, paths, and named commands from prose without inventing them |
| [Date extraction](https://docs.typesafe.ai/cookbooks/date_extraction_cookbook.md) | Read constituent parts; code validates and assembles | General pattern for semantic parsing into typed values |
| [Extraction cascade](https://docs.typesafe.ai/cookbooks/sde_cascade.md) | Cheap generation, focused semantic verification, selective escalation | Luna does work; specific unsupported claims receive further attention |
| [Confidence classification](https://docs.typesafe.ai/cookbooks/classification_using_confidence.md) | Return a broader useful category when exact classification is uncertain | Identify a subsystem even when the exact file is unresolved |
| [LLM guardrails](https://docs.typesafe.ai/cookbooks/llm_guardrails.md) | Separate checks around generated content | Focused output checks; never a replacement for runtime authority |
| [Noul consistency](https://docs.typesafe.ai/cookbooks/consistency_noul_cookbook.md) | Examine behavior around uncertain questions | Debug wording on revealing failures, not repeat until a desired answer wins |
| [Choice consistency](https://docs.typesafe.ai/cookbooks/consistency_choice_cookbook.md) | Inspect stability and competing alternatives | Improve boundaries and exits in a controller's action menu |

## Other published material to use alongside the recipes

- [Building guide](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md),
  [System One](https://docs.typesafe.ai/concepts/system-one.md),
  [state](https://docs.typesafe.ai/concepts/state.md), and
  [use-case map](https://docs.typesafe.ai/concepts/use-case-map.md): start with the
  application behavior and work backward to semantic decisions. Keep exploring
  non-swarm programs too.
- [Choice](https://docs.typesafe.ai/primitives/choice.md),
  [Score](https://docs.typesafe.ai/primitives/score.md),
  [Noul](https://docs.typesafe.ai/primitives/noul.md), and
  [advanced questions](https://docs.typesafe.ai/primitives/advanced.md): structured
  criteria, well-described alternatives, graded dimensions, and independent
  predicates provide more expressive building blocks than tiny flat prompts.
- [Speculative fan-out](https://docs.typesafe.ai/patterns/fan-out.md): ask useful
  conditional questions before knowing which branch applies. Questions in one
  request cannot consume one another's answers; new evidence creates a new stage.
- [Confidence](https://docs.typesafe.ai/confidence.md),
  [confidence routing](https://docs.typesafe.ai/patterns/confidence-routing.md), and
  [intent routing](https://docs.typesafe.ai/patterns/intent-routing.md): uncertainty
  can change specificity, evidence collection, or who continues the work.
- [Smart home](https://docs.typesafe.ai/demos/smart-home.md): speculative action
  parsing plus an LLM for compound-request decomposition or free-form responses.
  The page says source will be available at release; this reading did not verify
  a runnable public source repository.
- [Patterns index](https://docs.typesafe.ai/patterns.md),
  [demos](https://docs.typesafe.ai/demos.md), and
  [model jaggedness](https://docs.typesafe.ai/model-jaggedness/jev-1.13.md): continue
  discovery and record model/task-specific limits without treating them as eternal.

## Particularly useful combinations

1. **An investigation in one authored program.** Search produces candidates;
   hierarchical exploration preserves competing leads; reranking chooses reads;
   passage questions separate support from contradiction; typed action selection
   fetches the next observation. Return evidence or a concrete unresolved question.
2. **A better tool result.** Recover structure; ask several task-specific relevance
   questions; render selected original blocks and an expandable omission index.
   Save judgments for other consumers of the same output.
3. **A component that improves between runs.** A model authors the question bundle;
   execution records helpful and unhelpful outcomes; a later model proposes a
   revision; skill-style discovery helps another agent find and reuse it. Begin
   with files and Git rather than a component marketplace.
4. **A cheap worker with focused assistance.** A Luna produces a candidate;
   code obtains evidence; per-field/per-requirement checks identify unresolved
   claims; a controller fetches missing material or requests a narrow follow-up.
   Deterministic checks and runtime permissions remain with their existing owners.
5. **A semantic worktree actor.** Combine a concrete goal, observed worktree state,
   typed action catalogue, speculative questions, and a bounded loop. First use a
   small task with useful observable feedback. Wider branching and model spawning
   can follow once that loop actually works.

These are options for quick trials, not five prerequisites for the next run.

## Corrections worth carrying forward

- Passing a confidence policy means an answer is accepted, not that its selected
  alternative is approval. Route every alternative explicitly.
- The entity-alignment example rounds a numerical Score to an outcome. There are
  boundaries at half-integers despite no fitted threshold constant. A mean can
  hide disagreement between extremes; inspect that behavior before adopting it
  for consequential merging or collapsing distinct findings.
- The function dispatcher uses defaults for arguments outside supported closed
  sets. Do not market that as general unconstrained function-call generation.
- A broader fallback category is useful only if it is genuinely supported; merely
  taking the parent of an uncertain winner need not capture the other contenders.
- Classifying apparent instruction attempts does not establish a security boundary.
- The formatting example preserves words while changing whitespace/markup. Our
  original-offset retrieval guarantee must be implemented separately.
- Vendor thresholds, demonstrations, and illustrative model comparisons do not
  establish thresholds or performance for our workload.
- Generic next-action prediction failing in local trials does not rule out
  state-aware controllers with explicit action choices and observed consequences.

## Next pass / small trials

First finish any code-level reads needed for the chosen recipe, including linked
support modules rather than only notebook prose. Then adapt one or two rich
examples directly into project Haskell and use them during the next Sol task.
A useful first pairing is an investigation program and a structured tool-output
view; a typed action controller is equally legitimate if the task fits it better.
Do not freeze that choice before seeing the actual task and available surface.

Record the exact friction, what work disappeared from model turns, what still
needed judgment, and the smallest revision. Interview the user/agent and iterate.
No thousand-run campaign, permanent registry, new UI, or generalized controller
framework is required to learn whether these patterns help.
