# Tidepool

This repository contains two products built together:

- [Tidepool](tidepool/README.md) compiles and runs typed Haskell effect
  programs as resident Cranelift state machines.
- [Exomonad](exomonad/README.md) builds persistent agent applications,
  Codex integration, and programmable coordination on Tidepool.

Shared workspace infrastructure remains at the repository root. Mixed effect
contracts and handlers live under the transitional [`bridge/`](bridge/README.md) while their final
ownership is resolved.

**Your agent can program how it works.**

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE.md)

Give your agents a shared programming language for using tools, investigating
problems, and working together. In **Exomonad**, Tidepool’s agent environment, that
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
is out of RAM and your SSH daemon is dead. Exomonad gives the swarm an aggregate
resource boundary and individual commands their own budgets.

This is my Gastown. It is also an invitation to take a very programmable agent
environment and see what you can make it do.

## Put semantic judgment inside the program

Jev supplies typed judgments; Haskell combines them with exact operations and
whatever state the agent has kept. Ask several questions about the same evidence
in one request. Keep the answers around. Change how you use them without reading
the evidence into another large-model turn.

Here is a small example in the Exomonad workbench. These illustrative excerpts could
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

**What it costs.** One packet is one round trip, however many questions are in
it. Measured in a live session against `jev-1.13.0`: the six questions above
returned in 215 ms, and a packet of 322 questions over a whole source file
returned in 351 ms. So ask everything you want to know about one piece of
evidence at once; a second packet costs another round trip and a judgment cannot
see another judgment's answer anyway.

What is not cheap yet is compiling a cell, which is where this alpha spends its
time. That is the argument for moving a program you intend to run again out of
the notebook: a tool or an after-tool slot is compiled once when it is installed
and every later call runs retained machine code, so the same Jev-backed judgment
that costs you a compile as a cell costs a few hundred milliseconds as a tool.

The [Jev skill](exomonad/examples/workspace/.exomonad/skills/exomonad-jev/SKILL.md) covers
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

A project’s `.exomonad/` package contains Haskell modules, configuration, prompts,
and skills. Start from the [example package](exomonad/examples/workspace/README.md),
then reshape it around your project. Choose the models, write your coordination
rules, add useful functions. Experiment live; save the good parts as source for
the next run. The package is captured at launch, and an agent that edits its
modules can ask for them back: `reloadSource` typechecks the edited source and
publishes it in one step, or refuses and leaves the running notebook exactly as
it was. Existing bindings stay; later cells see the new code.

### An agent's own tools and reflexes

Each checkout carries one more module, `.exomonad/AgentSpec.hs`. It is the agent's
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

**Compiled once, then free.** Compiling Haskell is the slow part of this
system, and a notebook cell pays for it. A tool or a slot does not. It is
compiled when it is installed, and every call after that runs the retained
machine code in the agent's own resident machine. In a live session a tool that
ran two searches and asked Jev about the results answered in half a second, an
idle slot added about thirty milliseconds to a call, and a Jev packet of three
hundred questions came back in a third of a second. So the expensive thing is
writing a reflex, and running it ten thousand times costs next to nothing. That
is the trade the whole design makes: spend compile time once to stop spending
reasoning-model turns on the same judgment again and again.

**Same machine, same effects.** A tool body or a slot is not a sandboxed
plugin. It runs with the agent's own effect row, so it can run commands, ask
Jev, read the agent's recent conversation, send to a record actor, or message
another agent.

**A parent installs monitors on the subtrees it spawns.** This is the part we
are most excited about. A capable parent hands work to fast, cheap children, and
the usual price is that nobody is watching them. Here the parent writes the
watching down: a few lines of Haskell that ask Jev, after each of a child's tool
calls, whether it is repeating itself, leaving its assignment, ignoring a
failure, or about to do something destructive. Children are made from a commit,
so every child carries what the parent committed, and the parent chooses which
monitors each child gets by the label it spawns it under.

A parent writes two kinds of heuristic. A **nudge** is advice it already knows
the answer to: when it looks like the child is doing X, the child's next tool
result carries the parent's own line telling it to do Y instead, and the parent
spends nothing. An **escalation** is for what the parent must decide: it sends
the parent the reason and the handle of the node that tripped it, and the parent
steers that child directly with the tools it already has. Both run at Jev speed
inside the child, so the parent is only interrupted for what it has to answer.

The heuristics are ordinary values in the workspace, so a set of them is a list,
two sets compose with `<>`, and a parent adds one for the task at hand by
writing a sentence. Jev's own question packets compose the same way one level
up, with an append that carries both sets' labels in the type and rejects a
duplicate at compile time.

That whole capability is workspace Haskell. Designing it and writing it took
about half an hour. It needed two small additions to the shipped library, a way
for a slot to name its parent and an optional line of intent on a shell call,
and no engine change at all: asking Jev, reading the running actor's identity
and messaging another agent were already effects, and a slot already ran
compiled after every tool call.

This is what the system is for. The engine work is done once, in Rust and in the
effect contracts. After that a new reflex, a new tool, a new monitor over a whole
subtree is a few dozen lines in a file the agent itself can edit, typecheck and
reload without restarting anything. An agent that finds a better way to work can
write it down and be using it minutes later.

The agent edits the module with ordinary file tools and calls
`reload_agent_spec`. Tool bodies and the slot swap between calls; a call already
running keeps the code it started with. A reload that would change a tool's
name, description or schema is refused with the difference: the tool list is
registered once per session, so a changed surface waits for the agent's next
incarnation and the prompt already sent is never rewritten. The
[agent spec skill](exomonad/examples/workspace/.exomonad/skills/exomonad-agent-spec/SKILL.md)
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

You can also drive a running Exomonad session from a terminal or another coding
agent, keeping bindings across submissions:

```bash
exomonad proxy my-session experiment.hs
exomonad proxy my-session --actors
```

That is how we run many of the lab experiments. See the
[operator interface](exomonad/docs/operator-http.md).

## Make a swarm—or something else

Exomonad provides the pieces to build recursive coding swarms:

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

The [actor guide](exomonad/examples/workspace/.exomonad/skills/exomonad-define-actors/SKILL.md)
and [orchestration skill](exomonad/examples/workspace/.exomonad/skills/exomonad-orchestrate/SKILL.md)
show how to compose these pieces. The supplied workflow is a starting point.
A single agent with powerful cells, a collection of semantic background actors,
or an experiment unrelated to coding swarms can use the same substrate.

## Try it

The current setup is **Linux with Nix, systemd user services/cgroup v2,
Bubblewrap, and tmux**. Exomonad uses a pinned Tidepool Codex fork for its hosted
interface. Authenticate that client before starting model work. For Jev, set
`TYPESAFE_API_KEY` in the environment before launching Exomonad. The Jev operators
are [jev-dsl](https://github.com/inanna-malick/jev-dsl), compiled from the
revision your project's `flake.nix` pins; `exomonad new` writes that pin, and the
[workspace setup guide](exomonad/examples/workspace/README.md) explains it.

Build the matching host, extractor, and client from this checkout:

```bash
git clone --recurse-submodules https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#exomonad
./result/bin/exomonad --help
```

Nix 2.27 or newer is required so flake source capture includes the matched
Codex submodule. The first build compiles everything, GHC-side and Rust-side, and takes a good
while: Tidepool patches GHC to emit fat interface files, so the compiler and
every Haskell dependency are rebuilt from source. Later builds are incremental.
The wrapper selects the matched extractor and client without replacing `codex`
on your normal PATH.

### The binary cache

`flake.nix` declares a Cachix substituter that serves that toolchain prebuilt.
Whether you need to do anything depends on how Nix was installed:

```bash
nix config show trusted-users
```

- **Your username is listed** — this is what the Determinate Systems installer
  does — then nothing is needed. Accept the flake configuration when prompted
  and the cache is used.
- **Only `root` is listed**, the official multi-user installer's default, then
  one root action is needed once:

  ```bash
  sudo cachix use tidepool            # writes /etc/nix/nix.conf
  sudo cachix use tidepool --mode nixos   # on NixOS, writes /etc/nixos/cachix/
  ```

- **Single-user install** (no daemon), then `cachix use tidepool` without sudo.

A substituter writes into the shared `/nix/store`, so only root can authorize
one; a user who could add substituters and signing keys could hand every other
user on the machine an arbitrary binary. That is why a flake's own
`nixConfig` is ignored for untrusted users, and why no flag works around it.
On NixOS the least-privilege form is to permit rather than impose, leaving the
opt-in with the flake:

```nix
nix.settings.trusted-substituters = [ "https://tidepool.cachix.org" ];
nix.settings.trusted-public-keys = [
  "tidepool.cachix.org-1:jnYeaWymP+9/MeAECROfi4+/l7X1ilkOqM5Nrr5Lo1w="
];
```

Configure a systemd user slice with finite RAM and swap limits appropriate to
your machine. Exomonad defaults to `swarm.slice` and checks placement before running
payloads. Follow the [workspace setup guide](exomonad/examples/workspace/README.md)
for the project package and aggregate resource boundary.

From the repository you want to work on:

```bash
/path/to/tidepool/result/bin/exomonad new
/path/to/tidepool/result/bin/exomonad check --workspace .
/path/to/tidepool/result/bin/exomonad init
```

`exomonad new` writes the workspace package — configuration, the jev-dsl pin, the
Jev operators, a starter agent spec, and the skills an agent loads — into an
empty directory or a repository that has none, and commits or stages it.
`exomonad check` compiles that package without starting actors or providers; a live
run exercises execution and provider integration. `exomonad init` starts the run:
it opens a tmux session with the host, compiler, and root agent’s Codex TUI, and
scaffolds nothing.

Give it a task. Detach with `Ctrl-b d`, or launch with `--no-attach` and use the
printed connection information. Real agent runs and Jev calls use your accounts.

A good first task: ask the agent to investigate something in your repository,
use Jev where semantic judgment helps, and save one useful Haskell function for
its next task. Let it change the program as it learns.

Preserve useful code in Git. Restarting the host does not restore its old live
heap, jobs, or handles. [Getting started](exomonad/docs/getting-started.md) walks through
each command and what it writes.

### What it does not protect you from

Agents run arbitrary shell commands as you. The Bubblewrap boundary is write
containment, not a hardened sandbox: it keeps each agent's edits inside its own
checkout and makes the rest of the repository read-only, and the memory limits
stop a swarm from taking the machine down. The network, your credentials, your
environment and the rest of the host filesystem stay reachable. Run it on a
machine and in an account where that is acceptable, as you would any coding
agent with shell access.

Run status exposes recovery generation, predecessor-to-successor actor mappings,
lost state, unavailable actors, and resource-service health. Its resource
snapshot distinguishes active and historical commands, retained allocations,
cleanup failures, and bounded process, memory, pressure, CPU, and I/O
observations; traces also report output truncation. Existing cleanup and run-map
commands report retained build storage, and each run status contains a bounded
run-directory byte and entry count with an explicit truncation flag. These observations are diagnostic;
this recovery work adds no new limits. Stronger host isolation would require a
separate security boundary rather than extending the trusted local resource
service.

A run records a structured trace under `.exomonad/logs/`, and by default that
includes cell source, tool results and diagnostics in full, so anything a
command prints ends up there. The directory is ignored by Git. Set
`EXOMONAD_TRACE` to a narrower filter, for example `info`, to leave content out.

## What is Tidepool?

Underneath Exomonad is **Haskell running inside Rust**. GHC compiles the source,
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
[Haskell-backed tool](exomonad/examples/workspace/.exomonad/skills/exomonad-command/references/hosted-tools.md)
to the agent spec when a program should also be available through a tool
interface. For a new
host capability, define the effect contract in
[`tidepool-protocol`](bridge/protocol/README.md) and implement its Rust handler
in [`tidepool-handlers`](bridge/handlers/). Generated bindings connect the sides.

## Status and development

This is an early alpha: an experimental system you can use and reshape today.
Interfaces will change without notice. Setup is involved, and live use still
finds workbench papercuts. The most useful examples come from trying real tasks,
keeping what works, and fixing what gets in the way.

The sharpest edge today is compile latency. A first notebook cell in a fresh
session takes on the order of a minute, and reloading an edited agent spec about
half of that, because each statement is compiled and its machine code generated
from scratch. Everything downstream of a compile is fast: a tool call answers in
well under a second, an idle after-tool slot adds tens of milliseconds, and Jev
answers in a few hundred. We are working on the compile path, and the design
already lets you spend it once rather than every turn.

This is early alpha. Given another week on compiles-per-call and compile speed,
here is what I expect:

| Operation | Today | Expected |
| --- | --- | --- |
| Reload an edited spec | 28–35 s | 1–2 s |
| Warm cell, six statements | ~62 s | 2–3 s plus the effects themselves |
| First cell, fresh session, known workspace | 78 s | 3–5 s |
| First ever session in a new workspace | ~113 s | 20–30 s, once |
| Child joining a swarm | 7 m 45 s | 10–20 s |

Still, a 60-second compile that saves you a frontier-model round trip is worth
it today.

We are exploring agents exchanging and improving semantic functions, context-aware
tool views, and programs that do more work between model turns. These are directions
for experimentation, not a claimed speedup or a finished autonomous service.

Read [AGENTS.md](AGENTS.md) for contributor guidance and [plans/README.md](plans/README.md)
for active work. The main implementation areas are:

| Area | Source |
| --- | --- |
| Haskell library and extractor | [`bridge/haskell/`](bridge/haskell/) |
| Prepared execution and resident state | [`tidepool/codegen/`](tidepool/codegen/), [`tidepool/runtime/`](tidepool/runtime/) |
| Actors and workbench | [`exomonad/actor/`](exomonad/actor/) |
| Processes, resource controls, worktrees | [`exomonad/node/`](exomonad/node/), [`exomonad/worktree/`](exomonad/worktree/) |
| Providers and Exomonad host | [`exomonad/agent/`](exomonad/agent/), [`bridge/facade/`](bridge/facade/) |

Use focused checks while developing:

```bash
nix develop
just --list
just test-lib exomonad-actor 'test(your_test_name)'
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
