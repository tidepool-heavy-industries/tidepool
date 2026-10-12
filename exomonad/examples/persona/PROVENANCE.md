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

## Web retrieval

Tavily's [search](https://docs.tavily.com/documentation/api-reference/endpoint/search)
and [extract](https://docs.tavily.com/documentation/api-reference/endpoint/extract)
contracts informed the adapter.
The adapter requests basic retrieval and no generated answer. Semantic filtering
is authored with the existing pinned Jev DSL, after retrieval.
