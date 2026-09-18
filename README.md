# Tidepool

**Your agent can program how it works.**

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE.md)

Give your agents a shared programming language for using tools, investigating
problems, and working together. In **Shoal**, Tidepool’s agent environment, that
language is Haskell. Agents write programs as they go: keep values between turns,
define functions and types, launch work, and compose the results.

Put [**Jev**](https://docs.typesafe.ai/concepts/system-one) inside those programs
and things get interesting. Cheap semantic judgments become ordinary ingredients
in code. A cell can gather evidence, ask a whole collection of questions, follow
up on the answers, and return something useful—all between reasoning-model turns.
Agents can save and share the little System 1 programs they discover.

And they can fork. Children can branch into children, inheriting useful context
and working state. **Bubblewrap and copy-on-write filesystem layers** give those
branches isolated working environments without eagerly copying every build
artifact. Memory limits, process boundaries, fork budgets, cancellation, and
cleanup make recursive trees practical. Easy fan-out needs an answer to the
**Sorcerer’s Apprentice** problem: you wanted a swarm, and suddenly the machine
is out of RAM and your SSH daemon is dead. Shoal gives the swarm an aggregate
resource boundary and individual commands their own budgets.

This is my Gastown. It is also an invitation to take a very programmable agent
environment and see what you can make it do.

## Put semantic judgment inside the program

Jev supplies typed judgments; Haskell combines them with exact operations and
whatever state the agent has kept. Ask several questions about the same evidence
in one request. Keep the answers around. Change how you use them without reading
the evidence into another large-model turn.

Here is a small example in the Shoal workbench. These illustrative excerpts could
instead come from a search or a command; the question names the task explicitly:

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

The next expression can fetch the promising files, inspect callers, or prepare a
focused question for another agent. Turn that sequence into a function and call
it again on the next failure. The code is yours to change.

The [Jev skill](examples/shoal-workspace/.shoal/skills/shoal-jev/SKILL.md) covers
per-item question batteries, choices carrying executable actions, policies for
settling an answer, and reading uncertainty. The [TypeSafe cookbooks](https://docs.typesafe.ai/patterns) are a
rich source of things to try: semantic search, structured extraction, multi-path
exploration, and more. A judgment can be wrong; the program decides how to use it,
collect more evidence, or ask a reasoning model.

Our [live lab](plans/jev-lab/RESULTS.md) has been exploring programs that turn
failed builds into source investigations. One particularly useful
[experiment](plans/jev-lab/INTENT-EXPERIMENT.md): the same compiler errors imply
different repairs depending on whether a signature change was intentional.
Supplying the assignment resolved the ambiguity that threshold tuning could not.

## Your harness is a program

The inspiration is **XMonad and `xmonad.hs`**: configure and extend your working
environment in the language you use to operate it.

A project’s `.shoal/` package contains Haskell modules, configuration, prompts,
and skills. Start from the [example package](examples/shoal-workspace/README.md),
then reshape it around your project. Choose the models, write your coordination
rules, add useful functions. Experiment live; save the good parts as source for
the next run. The package is captured at launch, and an agent that edits its
modules can ask for them back: `reloadSource` typechecks the edited source and
publishes it in one step, or refuses and leaves the running notebook exactly as
it was. Existing bindings stay; later cells see the new code.

### An agent's own tools and reflexes

Each checkout carries one more module, `.shoal/AgentSpec.hs`. It is the agent's
`xmonad.hs`: the tools it is offered, and a slot that runs after every tool call.

```haskell
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just noted
  }

noted :: ToolCall -> ToolResult -> Eff effects Annotation
```

A tool is a field in a record, `mode :- Call input output`. The field name is
the tool name and the schemas derive from the types, so the declaration and the
handler cannot drift apart. The body is ordinary Haskell with the agent's own
effects, which means a tool can ask Jev before it answers.

The after-tool slot is where System 1 lives. It sees each finished call and its
result and may annotate it, prune it to the lines that matter while the whole
result stays bound to a name, or say nothing. It can read the recent
conversation and ask Jev what is relevant to it. It is compiled once with the
tools, so nothing compiles per call, and the reasoning model is only shown what
survived.

The agent edits the module with ordinary file tools and calls
`reload_agent_spec`. Tool bodies and the slot swap between calls; a call already
running keeps the code it started with. A reload that would change a tool's
name, description or schema is refused with the difference: the tool list is
registered once per session, so a changed surface waits for the agent's next
incarnation and the prompt already sent is never rewritten. The
[agent spec skill](examples/shoal-workspace/.shoal/skills/shoal-agent-spec/SKILL.md)
has the details.

An agent can use ordinary shell tools or write reusable command values:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,40p' -- "$1"|]
Cmd.run (preview "src/a file.rs")
```

Keep a long-running job and inspect it in a later cell:

```haskell
let check = withMemory (GiB 4) [bash|cargo test -p my_crate --lib|]
job <- Cmd.start check
-- Later, use the same job:
Cmd.status job
Cmd.tailOutput Cmd.Stderr job
```

Bindings hold actual values and handles, not a prose recollection of what happened.
Large outputs can stay outside the conversation until the agent needs a particular
part. The ordinary shell tools and Haskell commands use the same runtime owners.

You can also drive a running Shoal session from a terminal or another coding
agent, keeping bindings across submissions:

```bash
shoal proxy my-session experiment.hs
shoal proxy my-session --actors
```

That is how we run many of the lab experiments. See the
[operator interface](docs/SHOAL-OPERATOR-HTTP.md).

## Make a swarm—or something else

Shoal provides the pieces to build recursive coding swarms:

- **Typed assignments and replies.** `unfold` launches child actors and returns
  handles to their results and progress. Children can delegate in turn.
- **Useful inherited state.** Related agents can inherit conversation context;
  copy-on-write filesystem layers preserve useful source and build artifacts.
  Independent review can use a fresh context.
- **Programmable coordination.** Small Haskell actors retain state, receive events,
  collect replies, and run handlers without an LLM turn for each transition.
- **Owned worktrees and checked integration.** Workers edit isolated checkouts;
  parents review candidates and integrate through the worktree operations.
- **Resource controls.** Commands have memory budgets, cancellation, retained output,
  and input/PTY support. The shared systemd slice bounds the run as a whole.

A fork inherits a snapshot, not future messages. New decisions still need to reach
workers. Copy-on-write shares unchanged artifacts; new builds and writes still
cost RAM and disk. Set limits appropriate to your machine.

The [actor guide](examples/shoal-workspace/.shoal/skills/shoal-define-actors/SKILL.md)
and [orchestration skill](examples/shoal-workspace/.shoal/skills/shoal-orchestrate/SKILL.md)
show how to compose these pieces. The supplied workflow is a starting point.
A single agent with powerful cells, a collection of semantic background actors,
or an experiment unrelated to coding swarms can use the same substrate.

## Try it

The current setup is **Linux with Nix, systemd user services/cgroup v2,
Bubblewrap, and tmux**. Shoal uses a pinned Tidepool Codex fork for its hosted
interface. Authenticate that client before starting model work. For Jev, set
`TYPESAFE_API_KEY` in the environment before launching Shoal. The Jev operators
are [jev-dsl](https://github.com/inanna-malick/jev-dsl), compiled from the
revision your project's `flake.nix` pins; `shoal new` writes that pin, and the
[workspace setup guide](examples/shoal-workspace/README.md) explains it.

Build the matching host, extractor, and client from this checkout:

```bash
git clone https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#shoal
./result/bin/shoal --help
```

There is no public binary cache yet, so the first build compiles everything,
GHC-side and Rust-side, and takes a good while. Later builds are incremental. The wrapper selects the matched
extractor and client without replacing `codex` on your normal PATH.

Configure a systemd user slice with finite RAM and swap limits appropriate to
your machine. Shoal defaults to `swarm.slice` and checks placement before running
payloads. Follow the [workspace setup guide](examples/shoal-workspace/README.md)
for the project package and aggregate resource boundary.

From the repository you want to work on:

```bash
/path/to/tidepool/result/bin/shoal new
/path/to/tidepool/result/bin/shoal check --workspace .
/path/to/tidepool/result/bin/shoal init
```

`shoal new` writes the workspace package — configuration, the jev-dsl pin, the
Jev operators, a starter agent spec, and the skills an agent loads — into an
empty directory or a repository that has none, and commits or stages it.
`shoal check` compiles that package without starting actors or providers; a live
run exercises execution and provider integration. `shoal init` starts the run:
it opens a tmux session with the host, compiler, and root agent’s Codex TUI, and
scaffolds nothing.

Give it a task. Detach with `Ctrl-b d`, or launch with `--no-attach` and use the
printed connection information. Real agent runs and Jev calls use your accounts.

A good first task: ask the agent to investigate something in your repository,
use Jev where semantic judgment helps, and save one useful Haskell function for
its next task. Let it change the program as it learns.

Preserve useful code in Git. Restarting the host does not restore its old live
heap, jobs, or handles. [Getting started](docs/GETTING-STARTED.md) walks through
each command and what it writes.

### What it does not protect you from

Agents run arbitrary shell commands as you. The Bubblewrap boundary is write
containment, not a hardened sandbox: it keeps each agent's edits inside its own
checkout and makes the rest of the repository read-only, and the memory limits
stop a swarm from taking the machine down. The network, your credentials, your
environment and the rest of the host filesystem stay reachable. Run it on a
machine and in an account where that is acceptable, as you would any coding
agent with shell access.

A run records a structured trace under `.shoal/logs/`, and by default that
includes cell source, tool results and diagnostics in full, so anything a
command prints ends up there. The directory is ignored by Git. Set
`SHOAL_TRACE` to a narrower filter, for example `info`, to leave content out.

## What is Tidepool?

Underneath Shoal is **Haskell running inside Rust**. GHC compiles the source,
Tidepool extracts prepared STG, and Cranelift turns it into executable code driven
by Rust effect handlers.

The resident runtime gives agents a shared heap with scoped bindings. Precompiled
Haskell tools and live-authored programs use the same machinery; agents can define
types, keep functions and values, and share useful state through the actor and
fork interfaces. Sharing has scope and lifetime rules—it is not unrestricted
access to every other agent’s bindings.

Programs run through a typed effect stack. Haskell expresses the available
operations; Rust owns processes, providers, scheduling, permissions, and resources.
Runtime authority checks enforce access to concrete resources. The compiler is
what makes this interaction surface possible; agents simply write Haskell.

Tidepool was built with earlier iterations of this tooling and its predecessor
systems. We continue to develop it through agent-assisted runs and hands-on
experiments, feeding the failures back into the interface.

### Extend the surface

Write project functions and actors in Haskell. Add a
[Haskell-backed tool](examples/shoal-workspace/.shoal/skills/shoal-command/references/hosted-tools.md)
to the agent spec when a program should also be available through a tool
interface. For a new
host capability, define the effect contract in
[`tidepool-protocol`](tidepool-protocol/README.md) and implement its Rust handler
in [`tidepool-handlers`](tidepool-handlers/). Generated bindings connect the sides.

## Status and development

This is an early alpha: an experimental system you can use and reshape today.
Interfaces will change without notice. Setup is involved, and live use still
finds workbench papercuts. The most useful examples come from
trying real tasks, keeping what works, and fixing what gets in the way.

We are exploring agents exchanging and improving semantic functions, context-aware
tool views, and programs that do more work between model turns. These are directions
for experimentation, not a claimed speedup or a finished autonomous service.

Read [AGENTS.md](AGENTS.md) for contributor guidance and [plans/README.md](plans/README.md)
for active work. The main implementation areas are:

| Area | Source |
| --- | --- |
| Haskell library and extractor | [`haskell/`](haskell/) |
| Prepared execution and resident state | [`tidepool-codegen/`](tidepool-codegen/), [`tidepool-runtime/`](tidepool-runtime/) |
| Actors and workbench | [`tidepool-actor/`](tidepool-actor/) |
| Processes, resource controls, worktrees | [`tidepool-node/`](tidepool-node/), [`tidepool-worktree/`](tidepool-worktree/) |
| Providers and Shoal host | [`tidepool-agent/`](tidepool-agent/), [`tidepool/`](tidepool/) |

Use focused checks while developing:

```bash
nix develop
just --list
just test-lib tidepool-actor 'test(your_test_name)'
```

## License

Copyright Inanna Malick. Licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md):
free to use, modify, and share for any noncommercial purpose (personal
projects, research, education, charitable and government use included).
Commercial use requires a separate license — open an issue or contact the
author.

Versions published before this license change remain available under
their original MIT/Apache-2.0 terms via git history.
