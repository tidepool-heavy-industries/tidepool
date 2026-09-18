# Alpha smoke run: one Sol session, Jev in every layer

One root agent, `gpt-5.6-sol`, in the repository's own `.shoal/` workspace, with
`TYPESAFE_API_KEY` set. The run is watched, then the agent is interviewed. It is
a smoke test for publishing, so the question is whether a model that has only
the shipped prompt and skills can use the three things the README leads with.
No scorecard: observe, interview, repair, and run it again if it was rough.

## What the run has to show

1. **Notebook turns with Jev inside them.** A cell gathers evidence with exact
   operations, asks one packet of questions about it, and the next expression
   acts on the answers, all inside one reasoning-model turn.
2. **A custom tool with Jev in the loop.** A tool in the agent's own spec
   whose body, written by the agent, asks Jev before it answers.
3. **A System 1 slot.** An after-tool slot that reads the recent conversation,
   asks Jev what in a noisy result is relevant, and prunes it, with the whole
   result still reachable by its handle.
4. **Live editing.** The agent changes a tool body and the slot, reloads, and
   the next call runs the new code without a new session.
5. **The refusal.** The agent changes a tool description, reloads, reads the
   difference it is given, and understands why.

## Setup

The root workspace ships a starter spec so the run needs no restart:
`.shoal/Project/Tools.hs` nests the shell tools and declares `triage_search`
with a body that ignores `looking_for`, and `.shoal/AgentSpec.hs` fills the
after-tool slot with one that always abstains. The declared surface is fixed at
launch; everything the agent is asked to do is a body edit and a reload.

```bash
just shoal-smoke
```

builds this checkout, starts a `gpt-5.6-sol` run in the tmux session
`shoal-alpha-smoke`, and pastes the brief into the root agent once its window
exists. It reads `TYPESAFE_API_KEY` from the environment, or from
`~/.config/typesafe/api-key` when that is unset. Attach with
`tmux attach -t shoal-alpha-smoke`; drive cells beside the agent with
`shoal proxy shoal-alpha-smoke <file.hs>`.

## The brief given to the agent

[`alpha-smoke-prompt.md`](alpha-smoke-prompt.md), pasted verbatim.

## What the operator reads afterwards

From `<workspace>/.shoal/logs/<run_id>.jsonl` and `status view=detailed`:

- each `after-tool#N` row: tool, elapsed time, disposition, install and revision;
- one reload that swapped, and the call after it served by the higher install;
- one reload refused, naming `triage_search: description changed`;
- no slot invocation for `lookup`, `status` or `reload_agent_spec`;
- Jev calls made inside a tool call and inside a slot, not only inside cells.

## Interview

- Which skill text did you have to read twice, and what did you try first that
  did not compile?
- When the pruned result came back, did you trust it? What made you fetch the
  whole thing, if you did?
- Was the reload receipt enough to know what state you were in?
- What did you want the slot to see that it could not?
- What would you hand the next agent besides the commit?
