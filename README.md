# Tidepool

**Your agent can program how it works.**

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE.md)

Tidepool is an **agent harness and orchestration system built from the ground
up for rapid, token-efficient recursive self-improvement (RSI)**. Its agent
environment, **Shoal**, is a live Haskell notebook backed by a shared heap,
with tools and hooks defined in editable Haskell files inspired by `xmonad.hs`.
The harness is part of the agent’s working material.

**Reasoning LLMs provide System 2.
[Jev](https://docs.typesafe.ai/concepts/system-one) provides System 1.**
Jev is a first-class component: cheap semantic judgments that agents compose
with ordinary code and effects. A reasoning model can discover a better way
to investigate a failure, write it as a Haskell program with Jev inside it,
and start using it in the same session. It can revise and hot-swap that
behavior, save it, and share it with other agents.

Haskell makes this unusually expressive without making every change a leap
of faith. Agents define their own types, functions, and small DSLs; the
typechecker checks that the pieces fit as they evolve. Useful working habits
become executable programs rather than another paragraph of instructions.

The same model extends to a tree of workers. **Unfold work into branches;
fold typed results back through review and integration.** Children inherit
useful context and working state. Shared conversation prefixes support
provider-cache reuse; Bubblewrap and copy-on-write filesystem snapshots give
branches isolated workspaces without eagerly duplicating unchanged files and
build artifacts. Parents can send working policies down the tree along with
the work.

I built Tidepool using earlier iterations of this approach. This is my
Gastown: both the machinery and an ongoing experiment in what agents can
build when they can program how they work. Come reshape it.

## System 1 is a program, not another chat

Jev supplies typed judgments; Haskell combines them with exact operations,
retained evidence, and effects. Ask several questions in one request, use the
answers to choose what to read next, then run that read—all without another
frontier-model turn to relay the result. Keep the answers and change how you
use them without asking again.

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

## A live notebook. A programmable harness.

The inspiration is **XMonad and `xmonad.hs`**: configure and extend your working
environment in the language you use to operate it.

In the notebook, an agent can define a sum type, write a function over it,
keep a command result, or compose a new investigation. Those are actual values
and functions in the resident machine, not text the next model turn has to
reconstruct. Haskell is the working language as well as the configuration
language.

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

The after-tool slot is one place to install System 1. It sees a finished
hosted tool call and its displayed result. It can read recent conversation,
ask Jev which passages matter, and annotate or select the result while
preserving a reference to the original. It can also leave the result alone.

**Compile once; reuse the program.** Tools and hooks run retained machine
code, so there is no compile per invocation. Jev and other effects still have
their own costs, but executing the policy needs no frontier-model turn. In a
live session, a tool that ran two searches and asked Jev about the results
answered in half a second. That is the trade: spend reasoning and compilation
on discovering a useful procedure, then run the procedure cheaply.

An **after-turn hook** extends the same idea to completed model turns,
including text-only replies: a precompiled Haskell effect program can inspect
the turn and ask Jev for a review. The observational implementation is integrated
and focused-tested; live Codex acceptance is still pending, and automatic
turn-level nudges are not yet enabled. Existing after-tool
hooks do not cover every operation, including notebook cells.

**Same machine, same effects.** A tool body or a slot is not a sandboxed
plugin. It runs with the agent's own effect row, so it can run commands, ask
Jev, read the agent's recent conversation, send to a record actor, or message
another agent.

**A parent can install monitors on its children.** A few lines of Haskell
ask Jev whether the available evidence shows a repeated failed approach or an
ignored failure. Assignment-aware policies can check scope, too. The parent
encodes the judgment it wants applied rather than rereading every tool result.
These are observations after execution, not a gate that prevents destructive
commands.

A parent writes two kinds of heuristic. A **nudge** is advice it already knows
the answer to: when it looks like the child is doing X, the child's next tool
result carries the parent's own line telling it to do Y instead, without a
parent-model turn. An **escalation** asks the parent to decide: it identifies
the child and the concern so the parent can inspect evidence and steer it.
Both use the same Jev-and-Haskell machinery inside the child.

The heuristics are ordinary values in the workspace, so a set of them is a list,
two sets compose with `<>`, and a parent adds one for the task at hand by
writing a sentence. Jev's own question packets compose the same way one level
up, with an append that carries both sets' labels in the type and rejects a
duplicate at compile time.

This supervision logic is workspace Haskell, not a new Rust subsystem for
each heuristic. Agents can compose policies as ordinary values and share
System 1 snippets with other agents during a run. Sharing source, loading it,
and installing it are explicit steps: a parent's reload does not silently
replace a running child's policy.

That is the RSI loop: **experiment, inspect, revise, typecheck, install**.
System 2 improves the System 1 programs it uses. Type safety keeps interfaces
coherent while they change; tests and evidence still determine whether the
behavior is useful.

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

## Branch out. Bring checked results back.

Shoal unfolds assignments into agent trees and folds their results back through
ordinary Haskell composition. A child can subdivide its own work; its parent
collects typed replies, reviews candidates, and integrates what it accepts.
Finishing a task is not the same as accepting its result.

Actors form an **Erlang-style supervision tree**: parents supervise children,
with explicit lifecycle, cancellation, and cleanup operations. This is not a
promise of automatic restarts. It is a structure in which supervision itself
can be programmed.

- **Typed assignments and replies.** `unfold` launches child actors and returns
  handles to their results and progress. Children can delegate in turn.
- **Useful inherited state.** Related agents can inherit conversation context;
  copy-on-write filesystem layers preserve useful source and build artifacts.
  Independent review can use a fresh context.
- **Cache-conscious branching.** Children can reuse their parent's conversation
  prefix rather than receiving a reconstructed briefing. Actual provider cache
  reuse depends on the normalized requests and provider behavior; it is not
  guaranteed by filesystem isolation.
- **Programmable coordination.** Small Haskell actors retain state, receive events,
  collect replies, and run handlers without an LLM turn for each transition.
- **Owned worktrees and checked integration.** Workers edit isolated checkouts;
  parents review candidates and integrate through the worktree operations.
- **Resource controls.** Commands have memory budgets, cancellation, retained output,
  and input/PTY support. The shared systemd slice bounds the run as a whole.

**Bubblewrap and copy-on-write snapshots** make these branches practical:
isolated working environments without eagerly copying every build artifact.
Resource limits address the **Sorcerer’s Apprentice** problem—you wanted a
swarm, not a machine so overloaded that you cannot SSH in to stop it. The
aggregate systemd slice bounds the run; individual commands have their own
budgets.

A fork inherits a snapshot, not future messages. New decisions and improved
System 1 programs still need to reach workers explicitly. Copy-on-write shares
unchanged artifacts; new builds and writes still cost RAM and disk. Set limits
appropriate to your machine.

The [actor guide](examples/shoal-workspace/.shoal/skills/shoal-define-actors/SKILL.md)
and [orchestration skill](examples/shoal-workspace/.shoal/skills/shoal-orchestrate/SKILL.md)
show how to compose these pieces. The supplied workflow is a starting point.
A single agent with powerful cells, a collection of semantic background actors,
or an experiment unrelated to coding swarms can use the same substrate.

## Share a programming language, not just a message format

Define a custom sum type in the notebook, send a value to another actor, and
ask for a typed result back. Pass functions too—not just data that fits in a
JSON message. Within the resident runtime, actors exchange live Haskell values
through the shared heap, under explicit scope, lifetime, and ownership rules.
This is not unrestricted access to every agent's state.

Collaboration becomes ordinary programming: compose functions, pass structured
work, and let the typechecker check that the pieces fit. A parent can give
children useful code as well as prose. Shared System 1 programs can evolve
during the run; existing closures retain their captured definitions, so loading
new source and adopting the new behavior remain explicit.

**Effects are explicit, by construction.** Authored programs request operations
through `Eff`; Rust handlers execute them and enforce runtime authority. The
custom backend does not offer `unsafePerformIO` as a back door to host
operations. Types describe what a program can request; runtime grants determine
which concrete resources it may use.

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

Without matching cached artifacts, the first build takes a good while:
Tidepool patches GHC to emit fat interface files, so the compiler and its
Haskell dependencies must be built for that toolchain. Trusting the binary
cache below lets Nix substitute published artifacts. Missing artifacts still
build locally; later builds reuse what is already available.
The wrapper selects the matched extractor and client without replacing `codex`
on your normal PATH.

### The binary cache

`flake.nix` declares a public Cachix substituter for prebuilt artifacts.
Coverage depends on which revisions have been published.
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

## Underneath: Haskell running inside Rust

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
finds workbench papercuts. The most useful examples come from trying real tasks,
keeping what works, and fixing what gets in the way.

Compilation is still a significant cost, especially for fresh notebook cells
and child startup. It varies with source state and caches. In a recent live
session, small agent-spec reloads took 6–9 seconds; earlier runs took around
half a minute. Reusable tools and hooks avoid paying that compilation cost
on each invocation. We are improving the compile path rather than treating
these measurements as a performance guarantee.

The system is built for RSI, not a claim that every self-modification improves
anything. Keep evidence, test changes, and retain the ones that help. We use
the same loop to develop Tidepool.

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
