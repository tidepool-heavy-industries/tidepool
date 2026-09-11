# Tidepool

**A programmable swarm harness: Haskell as the agent interface, cheap copy-on-write forks underneath.**

[![CI](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml/badge.svg)](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE.md)

**Shoal**, Tidepool's swarm harness, makes Haskell the primary interface for
agents to act and coordinate. An agent can write directly in a persistent,
GHCi-style session or invoke a Haskell-defined tool with a familiar shell or
structured-input interface. Both run through the same effect stack and runtime
owners. Tools are compiled Haskell functions; orchestration is code the agents
can compose and revise as they work.

**Fork the reasoning and the working environment.** Related agents inherit useful
conversation context, while extensive use of Bubblewrap and OverlayFS gives them
isolated copy-on-write source and build views. A child can start with the parent's
warm artifacts instead of copying a huge build directory or rebuilding from
scratch at every branch. That makes recursive fork/implement/integrate cycles
practical for substantial development tasks.

Each interactive agent still has an ordinary Codex TUI. Behind it, persistent
Haskell bindings hold data, functions, jobs, and typed agent handles. Small
Haskell actors route events and collect results without spending an inference
turn on every mechanical step.

Tidepool supplies the compiler and runtime: GHC extracts the Haskell program,
Cranelift compiles it into an effect machine, and Rust drives it. Haskell
expresses behavior; Rust owns processes, providers, scheduling, permissions,
and resources.

## What works today

This is an experimental system under active development. We use it to develop
Tidepool itself through successive multi-agent runs, then feed observed failures
and friction back into the runtime and prompts. It is useful now, but not a
turnkey, unattended service.

- **Persistent working memory.** Bind data, functions, jobs, and agent handles
  once; use them in later turns. Filter large values in Haskell and return the
  parts that matter instead of repeatedly filling the conversation with raw data.
- **Recursive agent trees.** `unfold` composes children with typed assignments
  and results. Related work can inherit the parent's conversation context;
  independent review can start with selected fresh context.
- **Cheap workspace and build forks.** Bubblewrap establishes isolated process
  views; OverlayFS shares inherited filesystem layers and gives each child its
  own writable layer. Source and compiled artifacts travel together into the
  next development frontier without eagerly duplicating the whole tree.
- **Typed coordination actors.** Record-shaped APIs describe state, calls, and
  event handlers. Small Haskell state machines collect results and route messages;
  model turns remain available for engineering decisions.
- **Shell tools backed by Haskell.** Direct Bash, execution, stdin, and retained
  output tools share the same command effects as the Haskell workbench. Ordinary
  reads need no Haskell quotation; reusable command programs remain available.
- **Resource-controlled commands.** Jobs have explicit memory limits, retained
  identities, cancellation, PTY/input support, and bounded output. Command resource
  isolation keeps a build's OOM from automatically taking its parent TUI with it.
- **Project-authored tools and guidance.** Haskell defines tools and coordination
  helpers; Markdown supplies prompts and ordinary Codex skills. One repository-root
  `.shoal` package is selected for the swarm and frozen for that run.

Main contains the current orchestration/tooling baseline. The prepared-STG engine
replacement and further interactive-application/sleep work are developed on
separate branches; their plans are not claims of shipped functionality. See the
[active plan index](plans/README.md) for those boundaries.

## Fork deeply without starting cold

A recursive swarm needs more than parallel model calls. Its children also need
the source, dependencies, and build artifacts their parents just worked with.
Shoal uses Linux mount namespaces through Bubblewrap and copy-on-write layers
through OverlayFS for that working environment.

Inherited layers are shared; each child writes into its own layer. Unchanged
build artifacts can stay shared across descendants, while edits remain isolated
for review and integration. The same mounted workspace location also helps avoid
unnecessary path changes between parent and child. This supports multiple rounds
of scaffolding, forking, and consolidation rather than a single fan-out of cold
checkouts.

Copy-on-write makes the fork cheap, not every subsequent write: modifying
inherited files can require copying them into a writable layer, and genuinely
new artifacts still consume disk. Compiler inputs determine actual cache reuse.
The goal is to preserve useful working state across forks, with resource ownership
and cleanup handled by the runtime.

## Haskell directly, or Haskell behind a tool

In a Shoal Haskell session, commands are ordinary reusable values:

```haskell
let preview path = Cmd.withArguments [path] [bash|sed -n '1,40p' -- "$1"|]
Cmd.run (preview "src/a file.rs")
```

Dynamic arguments remain data rather than interpolated shell syntax. For a
long-running check, keep its job and carry on with other work:

```haskell
let check = withMemory (GiB 4) [bash|
  set -euo pipefail
  cargo test -p my_crate --lib
|]
job <- Cmd.start check
-- In a later turn, inspect the same job rather than rerunning the command.
Cmd.status job
Cmd.tailOutput Cmd.Stderr job
```

The direct `bash`, `exec_command`, `write_stdin`, and `read_output`
tools offer the ordinary shell interaction path. They invoke startup-compiled
Haskell handlers through the existing command runtime. The raw Bash handler is
essentially:

```haskell
\script -> Cmd.run (Cmd.bashCommand script) >> pure ""
```

A custom tool does not need a new Rust dispatch case. The
[tool-definition example](examples/shoal-workspace/.shoal/skills/shoal-command/references/hosted-tools.md)
shows raw-text and structured endpoints declared in one Haskell record.

Both interfaces retain jobs and bounded output. Waiting stops observing after its
foreground allowance; it does not implicitly cancel the process. Output omitted
from a display, output absent from a captured value, and output no longer retained
are distinct. Handles refer to the live owning runtime, not restart-persistent
process promises.

## Coordination you can program

One record defines a coordination actor's state and callable routes. Its handlers
use ordinary state operations; its client exposes typed endpoints:

```haskell
import GHC.Generics (Generic)

data Join mode = Join
  { joinState   :: mode :- State (Maybe Text, Maybe Int)
  , sourceReady :: mode :- Call Text NoReply
  , checksReady :: mode :- Call Int NoReply
  , joined      :: mode :- Call () (R.Reply (Maybe (Text, Int)))
  } deriving Generic

let definition = coordinationActor "integration-join" Join
      { joinState = (Nothing, Nothing)
      , sourceReady = \commit -> modify' (\(_, n) -> (Just commit, n))
      , checksReady = \n -> modify' (\(commit, _) -> (commit, Just n))
      , joined = \() -> gets (\(commit, n) -> (,) <$> commit <*> n)
      }
joiner <- R.start definition
let endpoints = R.client joiner
R.send (sourceReady endpoints) "abc123"
R.send (checksReady endpoints) 4
R.call (joined endpoints) ()
```

This runs in the supplied Shoal scope. An `Event input` field can instead attach
to a worker's progress, a typed response, or a command's completion. Handlers
receive events sequentially and retain state; sources can be mapped and combined.
See the [actor guide](examples/shoal-workspace/.shoal/skills/shoal-define-actors/SKILL.md)
for subscriptions, authority, replacement, and finishing actors.

The [curated project package](examples/shoal-workspace/README.md) builds a development
workflow on these primitives: plan with a human, establish shared contracts, let
leads recursively fork implementation work, review concrete candidates, and
integrate checked results. Its current examples use Astra for planning/hard
questions and Sol for execution. That division is authored guidance, not a
mandatory worker-stage pipeline.

Forking shares a snapshot of context, not all future knowledge. Changed decisions
must reach affected workers. Nor does a context fork guarantee a provider cache
hit: actual requests and usage are the evidence. The aim is to reuse useful
reasoning and build artifacts while keeping each subtree's task focused.

## Trying Shoal

The supported swarm setup is **Linux with Nix, systemd user services/cgroup v2,
Bubblewrap, and tmux**. Shoal uses a pinned Tidepool Codex fork for hosted tools
and interactive lifecycle integration. Authenticate that client for your account
before starting agents; real swarm runs consume model usage.

Build from the checked-out revision so the host, extractor, and native client
agree:

```bash
git clone https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#shoal
./result/bin/shoal --help
```

The Nix wrapper selects the matched extractor and Codex binary without replacing
`codex` on your normal PATH. Initial source builds can be substantial.

Before launching, configure a systemd user slice with finite RAM and swap limits
appropriate to your machine. Shoal defaults to `swarm.slice` and checks resource
placement before running payloads. The
[workspace setup guide](examples/shoal-workspace/README.md) describes package
selection and the aggregate resource boundary.

From the project you want agents to work on, use the built binary's absolute path:

```bash
/path/to/tidepool/result/bin/shoal init
```

Shoal opens a tmux session with a host, a shared compiler, and the root Codex TUI.
Give that root a task. Detach with `Ctrl-b d`; use `--no-attach` for a launch that
prints connection and status information. The root's first real prompt starts
inference.

For the curated planning/implementation workflow, prepare the
[example workspace package](examples/shoal-workspace/README.md) in your target
project first. It includes Haskell helpers, prompts, and repository skill links.
Validate a proposed package without launching agents:

```bash
/path/to/tidepool/result/bin/shoal check --workspace /path/to/project
```

A running swarm keeps its selected tools and prompts. Source edits take effect
at the next run boundary. Preserve work in Git for handoff; restarting a
conversation does not restore its old live Haskell heap or resource handles.

## The compiler and other interfaces

Shoal is one consumer of Tidepool. The repository also includes the one-shot
`tidepool` MCP server, the resident `tidepool-repl` MCP server, and Rust libraries
for embedding Haskell effect programs. These interfaces have different effect
handlers; an effect's type existing does not mean every host services it.

The current compilation path is:

```text
Haskell effect program → GHC Core → Tidepool IR → Cranelift effect machine
                                                        ↕
                                                  Rust handlers
```

| Area | Source |
|------|--------|
| Haskell library and extractor | [`haskell/`](haskell/) |
| IR, heap, compilation and resident sessions | [`tidepool-repr/`](tidepool-repr/), [`tidepool-codegen/`](tidepool-codegen/), [`tidepool-runtime/`](tidepool-runtime/) |
| Actor lifecycle and workbench | [`tidepool-actor/`](tidepool-actor/) |
| Processes, resources and managed checkouts | [`tidepool-node/`](tidepool-node/), [`tidepool-worktree/`](tidepool-worktree/) |
| Provider integration and Shoal launch | [`tidepool-agent/`](tidepool-agent/), [`tidepool/`](tidepool/) |
| Effect schemas and interpreters | [`tidepool-protocol/`](tidepool-protocol/), [`tidepool-handlers/`](tidepool-handlers/) |
| Embedding examples | [`examples/guess/`](examples/guess/), [`examples/tide/`](examples/tide/) |

Tidepool is not a full implementation of the GHC runtime. Supported Haskell
behavior is bounded by the extractor, IR, and runtime; compiler/runtime work is
ongoing. Prefer `Text` for substantial text data. Large values should be projected
before display, even when they remain available in the resident heap.

## Development

Read [AGENTS.md](AGENTS.md) for ownership and contributor guidance. Start with
focused checks for the code you change:

```bash
nix develop
just --list
just quick
# Example of a targeted crate check:
just test-lib tidepool-actor 'test(your_test_name)'
```

Boundary tests exercise the real Haskell workbench and native TUI with scripted
providers, including command output, retained jobs, PTY input, cancellation, and
OOM survival. Those fixtures need no paid inference. A passing fixture is evidence
for its tested boundary, not blanket acceptance of every ongoing swarm task.

## License

Licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE.md):
free to use, modify, and share for any noncommercial purpose (personal
projects, research, education, charitable and government use included).
Commercial use requires a separate license — open an issue or contact the
author.

Versions published before this license change remain available under
their original MIT/Apache-2.0 terms via git history.
