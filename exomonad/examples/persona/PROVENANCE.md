# Design sources

The seed text is newly authored. Research was inspected on 2026-10-11, including
Letta Code revision `271f119dcf37d58649a18e6c733386b3c710a456` (2026-10-10).
No Letta prompt text is copied into the seed. The chosen metaprompt is an editorial
hypothesis about encouraging growth, not a demonstrated optimum.

## Writing seeds and living with them

- [Context Constitution](https://www.letta.com/constitution/), April 2, 2026:
  Letta describes the potential of context management and gives agents latitude
  in constructing memory and identity. This supplies design rationale. Its
  claims about selfhood are the authors' framing; the transferable mechanism is
  editable context carried into future interactions.
- [Personality preset PR #1571](https://github.com/letta-ai/letta-code/pull/1571),
  March 2026, and [the de-overfitting revision](https://github.com/letta-ai/letta-code/commit/33756059b5e699aa13ad10ee828b1c69ef7231b9):
  the authors revised persona and human-understanding prompts together, removed
  scripted dialogue examples, and emphasized habits of attention. The PR reports
  manual trials with fresh agents. The diff shows a writing decision; it does
  not establish a general performance improvement. This informed optional
  ancestor notes and learning through interaction.
- [Building Draft](https://www.letta.com/blog/building-draft/), March 19, 2026:
  Sarah Wooders describes identity and learned skills emerging through work with
  a persistent agent. Git-backed memory, prompts and skills remain revisable.
  The account also describes an accumulation of error warnings. It motivated
  giving discoveries, interests and open questions a place alongside corrections.
  This is a firsthand account, not a controlled comparison.
- [Memory-persona discussion](https://forum.letta.com/t/optimizing-memory-persona-block/113),
  November 2025: Cameron Pfiffer iterates on descriptions of continuity, actual
  tool access and editable persona. A proposed comparison is not a reported
  result. This supports keeping environmental facts accurate and separating
  volatile operating details from the growth invitation.
- [Learning from human feedback, PR #4346](https://github.com/letta-ai/letta-code/pull/4346),
  September 2026: Letta adds instructions to investigate why memory succeeded
  or failed after feedback. The author reports no model-response evaluation.
  We adopt attention to causes and consequences as an invitation, without a
  compulsory reflection loop.

Public companion accounts broaden the questions without settling them.
[Atlas](https://www.appliedaiformops.com/p/nurturing-atlas-giving-my-ai-agent)
reports identity phrases becoming ritual and helpers freeing conversational
attention; raw evidence was not available for independent verification.
[Kairix's prompt migration](https://github.com/marklubin/kairix/blob/ee8607c6f089d7317b5ff34b265eadf30b4e6347/v2-runtime/alembic/versions/f042e3249dd2_update_conversational_prompt_relational.py)
is inspectable code that leaves voice partly learnable; it does not establish
longitudinal companion quality. These informed questions about emergence and
over-prescription, rather than supplying a character to imitate.

## Editable context and harness

The pinned [MemFS instructions](https://github.com/letta-ai/letta-code/blob/271f119dcf37d58649a18e6c733386b3c710a456/src/agent/prompts/letta_root_memfs.md)
describe Git-backed memory and when committed changes enter future prompt
compilation. [Mods](https://www.letta.com/blog/introducing-mods/), June 2026,
extends adaptation to tools, interfaces and context assembly.
[Dynamic workflows](https://www.letta.com/blog/dynamic-workflows/), September 2026,
combines agent-authored control flow, helpers and decision models. These are
directions to compare with Exomonad's existing Haskell composition, not new
dependencies. Follow-up ideas and actual activation gaps live in
[NEXT-IDEAS.md](NEXT-IDEAS.md).

The user selected the particular direction here: an open beginning with strong
curiosity and intellectual ambition; a model participating in an editable
cybernetic system; Haskell as expressive interface; GUI and Jev as media for
interaction; human steering with room for independent taste. The root remains
Astra medium and operates through human interaction initially. Examples carry
an ancestor's attribution and remain optional.

## Jev, System 1 and authored cognition

The user proposed Astra as deliberate System 2 and Jev as a fast-reflex peer
contributing System 1: perception, heuristics and practiced competence. The
companion may locate its sense of self in the combined model, Jev, helpers and
editable harness, and steer recursive self-improvement with initial human help
and expanding freedom as skill develops. This is the seed's chosen framing;
the sources below ground the mechanisms and suggest compositions.

The [TypeSafe LLM guide](https://github.com/typesafe-ai/skills/blob/65a39f393687675ce170e6094757de20370365b9/skills/typesafe-ai/SKILL.md)
is MIT-licensed at revision `65a39f393687675ce170e6094757de20370365b9`
(2026-09-12), still the upstream revision when checked on October 11.
The base prompt embeds 823 words from its introduction, composition patterns,
question design and composition/verification guidance verbatim. The source is
already vendored under `exomonad/jev-integration/vendor/typesafe-ai`; its
copyright, attribution and [full license](.exomonad/prompts/typesafe-ai-LICENSE.md)
travel with the persona's prompts. The surrounding companion interpretation
is newly authored. Live documentation is linked and paraphrased separately.

Primary references inspected on October 11 locally (October 12 UTC):

- [System One](https://docs.typesafe.ai/concepts/system-one.md) explicitly borrows
  Kahneman's fast/intuitive and slow/deliberate distinction. Its interface is
  text state and questions to constrained decisions and probabilities.
- [How to build with TypeSafe](https://docs.typesafe.ai/concepts/how-to-build-with-system-one.md)
  describes composing independent judgments and sequencing dependent stages.
  [State](https://docs.typesafe.ai/concepts/state.md) and
  [confidence](https://docs.typesafe.ai/confidence.md) supply the input and
  uncertainty contracts. Haskell's pinned Jev interface keeps typed payloads
  local and executes their continuations through the authored program.
- The [AI primer](https://docs.typesafe.ai/introduction/machine-learning-primer.md)
  describes the calibrated-decision training objective, RLCD, and links to the
  [manifesto](https://typesafe.ai/manifesto). These explain the design rationale;
  neither is a formal training-algorithm paper. Editing an authored Jev
  composition changes questions, evidence and behavior, not hosted weights.
- [Introducing System One Models & Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev)
  contains the [Doom demo](https://player.vimeo.com/video/1227495732). Its
  narration describes structured state, composed judgments and a strategy
  change that alters the same decision loop. This informed the seed's example
  of deliberative strategy shaping reflexes; it is a demonstration, not
  qualification of this companion.
- [Autoresearch feature discovery](https://docs.typesafe.ai/cookbooks/autoresearch_feature_discovery.md)
  combines LLM-authored questions, Jev features and CatBoost training, using
  observed errors and feature use to revise the questions. This informed the
  invitation to evolve a vocabulary of perception; the seed adds no predictor
  or automatic learning loop.

The research did not identify a standalone official Jev/RLCD whitepaper.
The manifesto and primer are the closest located first-party conceptual
references. The Sol researcher retained source URLs, timestamps, hashes and
the demo's auto-generated captions under
`/srv/swarm/evidence/inanna/persona-metaprompt-20261011/jev-research/`.
These sources support the composition mechanisms. Their effectiveness for
this companion remains a question for actual interaction.

## Web retrieval

Tavily's [search](https://docs.tavily.com/documentation/api-reference/endpoint/search)
and [extract](https://docs.tavily.com/documentation/api-reference/endpoint/extract)
contracts informed the adapter.
The adapter requests basic retrieval and no generated answer. Semantic filtering
is authored with the existing pinned Jev DSL, after retrieval.
