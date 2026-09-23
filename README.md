# Tidepool

**Your agent can program how it works.**

[![License: PolyForm Shield 1.0.0](https://img.shields.io/badge/license-PolyForm%20Shield%201.0.0-blue.svg)](LICENSE.md)

This repository builds two products in one Rust and Nix workspace:

- [Tidepool](tidepool/README.md) compiles typed Haskell effect programs into
  Cranelift-backed state machines driven from Rust.
- [Exomonad](exomonad/README.md) runs persistent agent applications, typed
  tools, and coordination on Tidepool.

The transitional [`bridge/`](bridge/README.md) holds shared and mixed
components whose final ownership is not yet resolved.

An agent shouldn't need a new reasoning turn to repeat a procedure it already
knows. It should be able to write the procedure down as code — including the
parts that need judgment — and run it. Exomonad's agent environment is a live
Haskell notebook, backed by a shared heap on the Tidepool runtime, where an
actor application composes commands, semantic judgments, and delegation into
programs, then installs the useful ones as tools, hooks, or coordination
policies without leaving the session.

**Reasoning LLMs provide System 2.
[Jev](https://docs.typesafe.ai/concepts/system-one) provides System 1.**
Jev supplies cheap, typed semantic judgments inside ordinary code. The
reasoning model designs and improves the procedure; the procedure handles the
recurring work. Useful habits become executable behavior rather than another
paragraph of instructions.

That makes recursive self-improvement a concrete development loop:
**experiment, inspect, revise, typecheck, install**. The object of improvement
is the agent's own harness. Save the useful programs for future runs and pass
them to other agents.

The same model extends to teams: **unfold assignments into workers; fold
typed results back through review and integration.** Parents can give
children code as well as instructions and supervise them without a model turn
for every event.

This is an early alpha for people who want to build and reshape agent
workflows. [Try it](#try-it) on Linux with Nix.

## System 1 is a program, not another chat

Use ordinary code for exact decisions: did the command succeed, does the file
exist, has the worker replied? Use Jev where the decision depends on meaning:
is this evidence relevant, does it explain the failure, which follow-up fits
the assignment?

Both belong in the same program: feed command output into a Jev judgment, then
use the answer to select another command or assign a worker. Control flow
stays in Haskell instead of bouncing through the conversation.

Here is a workbench example. The excerpts are illustrative; in an
investigation they could come from a search or command output:

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let excerpts =
      [ ("retry", "src/Retry.hs: retries failed requests with exponential backoff" :: Text)
      , ("fetch", "src/Fetch.hs: HTTP requests with a per-request timeout")
      , ("view",  "src/View.hs: renders the download progress indicator")
      ]
let packet =
      #per_file := J.each fst (\(name, text) ->
            #on_path := J.noul ("Could the code in " <> name <> " participate in the repeated request timeout? " <> text)
              :& #enough := J.noul ("Does this excerpt of " <> name <> " show enough implementation to diagnose the timeout? " <> text))
        excerpts
answer <- J.ask
  (J.state (#task := ("Investigate a download that times out after three retries" :: Text)))
  packet
fmap (\r -> [(name, a.on_path.yes, a.enough.yes) | ((name, _), a) <- r.per_file]) answer
```

This asks six questions in one request: is each excerpt relevant, and is it
sufficient? Those are different decisions. Relevant but insufficient evidence
calls for a deeper read; irrelevant evidence need not fill the conversation.
The returned probabilities are values the next expression can use — not a
chat reply that another model has to parse.

### Discover once, reuse cheaply

One packet is one round trip; batch independent questions about the same
evidence, and use another request only when a question needs an earlier
answer.

Fresh notebook cells pay compilation cost. Installed tools and hooks run
retained machine code: no compilation per invocation, and no reasoning-model
turn to execute the procedure. Spend reasoning and compilation once to
develop a procedure, then reuse it cheaply. Handle routine outcomes in code
and reserve the reasoning model for cases that need it.

The [Jev skill](exomonad/examples/workspace/.exomonad/skills/exomonad-jev/SKILL.md)
covers question batteries, choices carrying actions, uncertainty, and
settlement policies. The [TypeSafe cookbooks](https://docs.typesafe.ai/patterns)
explore further patterns.

## A live notebook. A programmable harness.

The notebook is a working environment, not a sequence of isolated scripts.
Bindings retain actual values, functions, and handles across cells. An agent
can keep command evidence, define a type for the cases it discovers, and
write functions over that type as the investigation develops.

Haskell lets the agent build abstractions while it works: a one-off
expression can become a function, a policy, or a small DSL without leaving
the language. The typechecker enforces interfaces across commands,
judgments, and actors as those abstractions evolve.

Commands are ordinary values too:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,40p' -- "$1"|]
Cmd.run (preview "src/a file.rs")
```

Long-running work has a handle that later cells can inspect:

```haskell
let check = withMemory (GiB 4) [bash|cargo test -p my_crate --lib|]
job <- Cmd.start check
-- Later, inspect the same job:
Cmd.status job
Cmd.tailOutput Cmd.Stderr job
```

Large outputs stay retained outside the conversation until needed. Shell
tools and Haskell commands use the same runtime owners and resource
controls.

### From experiment to installed behavior

A workspace's `.exomonad/` package contains Haskell modules, configuration,
prompts, and skills. Start with the
[example package](exomonad/examples/workspace/README.md), then add project
functions, model choices, and coordination policies.

`reloadSource` typechecks edited modules and publishes them atomically. On
rejection, the running notebook stays unchanged. Existing bindings remain;
later cells see the new source.

An agent's `.exomonad/AgentSpec.hs` defines its tools and hooks. This excerpt
installs an after-tool handler:

```haskell
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just noted
  }

noted :: ToolCall -> ToolResult -> Eff effects Annotation
```

Tool schemas derive from their input and output types. Tools, hooks, and
notebook programs use the same effects: run commands, ask Jev, inspect
recent conversation, send events to record actors, or message workers. A
procedure developed in a cell can become part of the harness without being
rewritten in a different extension language.

See the [agent-spec skill](exomonad/examples/workspace/.exomonad/skills/exomonad-agent-spec/SKILL.md)
and [hosted tools reference](exomonad/examples/workspace/.exomonad/skills/exomonad-command/references/hosted-tools.md).

### Programmable supervision

A parent can install Jev-backed policies that notice repeated failed
approaches, ignored failures, or work outside the assignment. A **nudge**
attaches advice to the child's next tool result; an **escalation** brings the
concern to the parent for a decision. Neither requires the parent model to
reread every tool result. These are observations after execution, not
preventative security gates.

Policies are ordinary Haskell values. Combine lists with `<>`, add a
task-specific scope check, or give reviewers and implementers different
policies. Jev question packets also compose, with duplicate labels rejected
at compile time. A new heuristic is a program, not a feature request for the
host.

### What survives a change?

| Operation | What it preserves |
| --- | --- |
| Another notebook cell | Live bindings, evidence, functions, and handles |
| Source reload | Existing bindings; later cells use the new source |
| Fork | A snapshot of inherited context and working state, not future updates |
| Host restart | Saved source, not the old live heap, jobs, or handles |

Existing closures retain their captured definitions. Sharing source, loading
it, and adopting a new policy are explicit steps; a parent's reload does not
replace a running child's behavior.

You can also submit cells from a terminal or another coding agent:

```bash
exomonad proxy my-session experiment.hs
exomonad proxy my-session --actors
```

The [operator interface](exomonad/docs/operator-http.md) keeps bindings
across submissions.

## Branch out. Bring checked results back.

Exomonad's Erlang-style supervision gives parents ownership of child
lifecycle, cancellation, and cleanup. Failure handling is authored policy:
parents decide how to respond rather than relying on automatic restarts.

- **Typed work.** Context unfold launches assignments and returns handles to
  replies and progress. Children can delegate in turn.
- **Cache-conscious context.** Related workers inherit the parent's
  conversation prefix instead of a reconstructed briefing, preserving shared
  input for provider caching. Independent reviewers can start fresh.
- **Isolated checkouts.** Bubblewrap and copy-on-write snapshots preserve
  useful source and build artifacts without eagerly duplicating them.
- **Event-driven coordination.** Haskell record actors collect events, retain
  state, and run handlers without a model turn for every transition.
- **Checked integration.** Parents review candidates and integrate accepted
  work; completion, review, and merge are separate steps.
- **Bounded resources.** Commands have memory budgets, cancellation, retained
  output, and input/PTY support. A systemd slice bounds the whole run.

The [actor guide](exomonad/examples/workspace/.exomonad/skills/exomonad-define-actors/SKILL.md)
and [orchestration skill](exomonad/examples/workspace/.exomonad/skills/exomonad-orchestrate/SKILL.md)
show how to compose these pieces. The same substrate supports one agent with
powerful cells, semantic background actors, or a tree of coding workers.

## Share a programming language, not just a message format

An agent can define a sum type in a cell and use it as the result of another
actor's work. It can pass a function, not just describe what the function
should do. Collaboration becomes programming: structured assignments,
executable policies, and typed results that the parent can compose.

Within the resident runtime, actors exchange these live Haskell values
through a shared heap, with scope, lifetime, and ownership enforced by the
runtime.

**Effects are explicit.** Haskell programs request operations through `Eff`;
Rust handlers execute them and enforce runtime authority. Passing a function
or handle does not confer permission to use its resources. The custom
backend provides no `unsafePerformIO` escape hatch to host operations.

## Try it

You need **Linux, Nix, systemd user services/cgroup v2, Bubblewrap, and
tmux**. Exomonad drives a pinned Tidepool fork of the Codex client as each
agent's interface, so authenticate that client before model work. For Jev,
set `TYPESAFE_API_KEY` before launching.

```bash
git clone --recurse-submodules https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#exomonad
./result/bin/exomonad --help
```

The wrapper selects the matching host, extractor, and client without
replacing `codex` on your normal PATH. Building from source compiles both the
GHC-side and Rust-side toolchains and can take a good while the first time.

### The binary cache

The flake declares a Cachix substituter for prebuilt artifacts. Follow the
[binary-cache setup](exomonad/docs/binary-cache.md) to authorize
substitutions for your Nix installation; coverage varies by revision.

### Start a session

Before launching, configure the finite RAM and swap limits described in
[getting started](exomonad/docs/getting-started.md). Exomonad defaults to
`swarm.slice` and checks placement before running payloads.

Then, from the repository you want to work on:

```bash
/path/to/tidepool/result/bin/exomonad new
/path/to/tidepool/result/bin/exomonad check --workspace .
/path/to/tidepool/result/bin/exomonad init
```

- `new` scaffolds a workspace package: configuration, the pinned
  [jev-dsl](https://github.com/inanna-malick/jev-dsl) operators, a starter
  agent spec, and the workspace skills.
- `check` checks workspace customization without launching native workers or
  providers.
- `init` starts the host, compiler, and root agent's Codex TUI in tmux.

Detach with `Ctrl-b d`, or use `--no-attach` and the printed connection
information. Model and Jev calls use your accounts.

From a development checkout of this repository, without a Nix build,
`just exomonad-init -- --workspace /path/to/repo` builds the matched tools
incrementally and starts the same run against that repository after its
`exomonad new`; `just exomonad-harness` is that recipe aimed at
`~/dev/exomonad-harness`.

For a first task, ask the agent to investigate a real repository issue,
retain its evidence, and save a useful investigation function. Then ask it
to reuse that function. This exercises the central loop, not just the chat
interface. [Getting started](exomonad/docs/getting-started.md) explains
authentication, configuration, and what each command writes.

### What it does not protect you from

Agents run arbitrary shell commands **as you**. Bubblewrap provides write
containment for agent checkouts, not a hardened security sandbox. The
network, your credentials, environment, and the rest of the host filesystem
remain reachable. Use an account and machine where that is acceptable.

By default, `.exomonad/logs/` records cell source, tool results, and
diagnostics in full — including secrets a command prints. The directory is
Git-ignored, not redacted. Set `EXOMONAD_TRACE` to a narrower filter such as
`info` to omit content.

## Underneath: Haskell running inside Rust

GHC compiles the source; Tidepool extracts prepared STG; Cranelift turns it
into executable code driven by Rust effect handlers. Live notebook programs
and precompiled tools use the same resident runtime.

Haskell owns authored programs and policies. Rust owns processes, providers,
scheduling, permissions, and resources. Agents change how those capabilities
are composed; the runtime remains responsible for enforcing their
boundaries.

### Extend the surface

Write project functions and actors in Haskell. Expose a reusable program as a
[Haskell-backed tool](exomonad/examples/workspace/.exomonad/skills/exomonad-command/references/hosted-tools.md)
when it should appear in the agent's tool interface. For a new host
capability, define its contract in [`tidepool-protocol`](bridge/protocol/README.md)
and its Rust handler in [`tidepool-handlers`](bridge/handlers/); generated
bindings connect the sides.

## Status and development

Early alpha: interfaces will change, setup is involved, and real tasks still
find workbench papercuts. Fresh cells and child startup pay significant
compilation cost; installed tools and hooks avoid that cost per invocation.

Bring a real task, build a better way to do it, and make that part of the
harness. That is how we develop Tidepool.

See [AGENTS.md](AGENTS.md) for contributor guidance and
[plans/README.md](plans/README.md) for active work.

| Area | Source |
| --- | --- |
| Haskell library and extractor | [`bridge/haskell/`](bridge/haskell/) |
| Prepared execution and resident state | [`tidepool/codegen/`](tidepool/codegen/), [`tidepool/runtime/`](tidepool/runtime/) |
| Actors and workbench | [`exomonad/actor/`](exomonad/actor/) |
| Coding-agent backend and managed worktrees | [`exomonad/agent/`](exomonad/agent/), [`exomonad/worktree/`](exomonad/worktree/) |
| Public facade and Exomonad host | [`bridge/facade/`](bridge/facade/), [`tidepool/`](tidepool/) |

Use focused checks while developing:

```bash
nix develop
just --list
just test-lib tidepool-runtime 'test(<name>)'
```

## License

Copyright Inanna Malick. **Source-available under [PolyForm Shield 1.0.0](LICENSE.md).**

Use Tidepool and Exomonad for personal projects or at work, including
commercial work, provided your use does not provide a competing product or
service. You may modify and share it under the license's terms. Competing
offerings need separate permission; contact the author to discuss commercial
licensing.

Previously published versions retain the terms under which they were
released.
