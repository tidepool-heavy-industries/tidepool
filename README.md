# Tidepool

**Your agent can program how it works.**

[![CI](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml/badge.svg)](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml)
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
let files = J.pool #files [(name, String text, name) | (name, text) <- excerpts]
let packet =
      #files := files
        :& #per_file := J.eachIn files (\file ->
              #on_path := J.askAbout file "Could this code participate in the repeated request timeout?"
                :& #enough := J.askAbout file "Does this excerpt show enough implementation to diagnose the timeout?"
                :& J.Nil)
        :& J.Nil
answer <- J.ask
  (J.state (object ["task" .= ("Investigate a download that times out after three retries" :: Text)]))
  packet
fmap (\r -> [(name, a.on_path.yes, a.enough.yes) | (name, a) <- (J.answers r).per_file]) answer
```

The next expression can fetch the promising files, inspect callers, or prepare a
focused question for another agent. Turn that sequence into a function and call
it again on the next failure. The code is yours to change.

The [Jev skill](examples/shoal-workspace/.shoal/skills/shoal-jev/SKILL.md) covers
pools, choices carrying executable actions, speculative questions, and reading
uncertainty. The [TypeSafe cookbooks](https://docs.typesafe.ai/patterns) are a
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
the next run. The selected package is frozen at launch, so file changes take
effect on the next run; live cells extend the running workbench.

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
`TYPESAFE_API_KEY` in the environment before launching Shoal.

Build the matching host, extractor, and client from this checkout:

```bash
git clone https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#shoal
./result/bin/shoal --help
```

Initial source builds can be substantial. The wrapper selects the matched
extractor and client without replacing `codex` on your normal PATH.

Configure a systemd user slice with finite RAM and swap limits appropriate to
your machine. Shoal defaults to `swarm.slice` and checks placement before running
payloads. Follow the [workspace setup guide](examples/shoal-workspace/README.md)
for the project package and aggregate resource boundary.

From the repository you want to work on:

```bash
/path/to/tidepool/result/bin/shoal init
```

This opens a tmux session with the host, compiler, and root agent’s Codex TUI.
Give it a task. Detach with `Ctrl-b d`, or launch with `--no-attach` and use the
printed connection information. Real agent runs and Jev calls use your accounts.

A good first task: ask the agent to investigate something in your repository,
use Jev where semantic judgment helps, and save one useful Haskell function for
its next task. Let it change the program as it learns.

Validate a workspace package before launching:

```bash
/path/to/tidepool/result/bin/shoal check --workspace /path/to/project
```

This checks the package; a live run exercises execution and provider integration.
Preserve useful code in Git. Restarting the host does not restore its old live
heap, jobs, or handles.

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
when a program should also be available through a tool interface. For a new
host capability, define the effect contract in
[`tidepool-protocol`](tidepool-protocol/README.md) and implement its Rust handler
in [`tidepool-handlers`](tidepool-handlers/). Generated bindings connect the sides.

## Status and development

This is an experimental system you can use and reshape today. Setup is involved,
and live use still finds workbench papercuts. The most useful examples come from
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

Licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE.md):
free to use, modify, and share for any noncommercial purpose (personal
projects, research, education, charitable and government use included).
Commercial use requires a separate license — open an issue or contact the
author.

Versions published before this license change remain available under
their original MIT/Apache-2.0 terms via git history.
