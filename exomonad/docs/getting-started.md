# Getting started with Exomonad

Two commands do two different jobs. `exomonad new` creates a project workspace
with a pinned shared-source submodule. `exomonad init` starts a run in a project
that has one. Everything else on this page is what happens around those two.

## What you need

Linux, with Nix 2.27 or newer, systemd user services on cgroup v2, Bubblewrap and tmux. Exomonad
drives a pinned Tidepool fork of the Codex client as each agent's interface, so
authenticate that client before starting model work. For Jev, put a TypeSafe
key in `TYPESAFE_API_KEY` before launching.

Agents run shell commands as you. Read
[what Exomonad does not protect you from](../README.md#what-it-does-not-protect-you-from)
before pointing it at anything you care about.

### One-time machine setup: the resource slice

Every run is placed in one systemd user slice, `swarm.slice` by default, so a
swarm has an aggregate memory ceiling however many agents it grows. Give the
slice finite RAM and swap limits before the first run. Exomonad checks placement
before it executes anything and records the limits it found in
`resource-budget.json` in the run directory. `[launch] systemd_slice` in the
workspace config names a different slice.

## Build

For Tidepool development, clone the source and use the local recipes. They enter
the Nix shell for dependencies and compile local source into
`bridge/haskell/dist-newstyle/` and `target/`, which Cabal and Cargo reuse on
later builds:

```bash
git clone --recurse-submodules https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
just exomonad-build          # incremental build only
just exomonad-init           # incremental build, then start a run here
```

Pass `exomonad init` flags through the second recipe, for example
`just exomonad-init -- --no-attach`. To start a run in another project with the
locally built tools, use `just exomonad-init -- --workspace /path/to/project`.

For an isolated distribution build, run `nix build .#exomonad` and use
`./result/bin/exomonad`. There is no public binary cache yet, so the first
build compiles everything, GHC-side and Rust-side, and takes a good while.
Nix reuses outputs for identical inputs, while a source edit creates a new
input hash and does not reuse this checkout's Cabal and Cargo incremental
outputs. The wrapper selects the matched extractor and client itself; it does
not replace `codex` on your `PATH`.

The matched Codex package uses its `local` Cargo profile: no LTO, no debug
information, and unoptimized code with release runtime semantics. This reduces
compiler memory and build work; runtime throughput may be lower than a release
build. An optimized distribution build remains available with
`nix build ./vendor/codex#codex-rs-release`.

## `exomonad new`: write a workspace

```bash
cd /path/to/your/project
/path/to/tidepool/result/bin/exomonad new
```

With a local development build, use `/path/to/tidepool/target/debug/exomonad
new` instead.

`exomonad new` takes an empty directory, which it makes a Git repository, or an
existing repository that has no `.exomonad/config.toml`. It refuses, and changes
nothing, when a workspace is already there. It writes:

| Path | What it is |
|---|---|
| `.exomonad/config.toml` | generated model defaults, source roots (`.` and `workspace`), modules, checks, and the jev-dsl flake source |
| `.exomonad/AgentSpec.hs` | generated project agent spec: tool declarations and the after-tool hook |
| `.exomonad/prompts/`, `.exomonad/plans/` | project-owned starter prompts and plans |
| `.exomonad/workspace/` | pinned Git submodule from `tidepool-heavy-industries/exomonad-default-workspace` with shared Haskell modules, checks, and skills |
| `.agents/skills/` | links into `.exomonad/workspace/skills/`, where the client looks for skills |
| `flake.nix`, `flake.lock` | only when the project has none: one input, pinning jev-dsl |

The generated files and submodule pin belong in Git. After cloning, a teammate
runs `git submodule update --init --recursive` and then `exomonad init`.

Jev's operators are compiled from the jev-dsl revision the project's `flake.nix`
pins. When the project already has a `flake.nix`, `exomonad new` leaves it alone
and prints the input line to add and the lock command to run. Until the pin
resolves, a session still starts, without `J`, and the agent is told so.

Child agents are launched from committed checkouts. Commit the package before
you ask an agent to delegate.

## Migrating an existing workspace lookup

`lookup` is now supplied by the workspace agent spec rather than installed
as a special hosted tool. Existing workspaces must provide `Project.Lookup`
and register its tool in their `Project.Tools` and `AgentSpec`, following the
shared workspace's `Project.Tools`.
Use the argument object `{"queries": ["name", "Module.name"]}`.
The tool preserves original lookup results and may add up to four related
declarations selected by Jev. Programmatic `Introspection.info` and `typeOf`
remain raw. Run `exomonad check` before reloading the agent spec.

## `exomonad check`: typecheck the workspace without starting anything

```bash
exomonad check --workspace /path/to/your/project
```

This resolves the pinned sources and typechecks every module the config names.
It launches no agents and makes no model calls. `--recipes` also runs the
workspace's own model-free recipe checks, if it declares any.

## `exomonad init`: start a run

```bash
cd /path/to/your/project
exomonad init
```

`exomonad init` scaffolds nothing. With no workspace it stops and says to run
`exomonad new`. With one, it captures the workspace, starts a tmux session named
after the project, and prints where things are:

```
log:     …/.exomonad/logs/<run>.jsonl
status:  ~/.cache/tidepool/exomonad/runs/<run>/status.json
attach:  tmux attach -t exomonad-<project>
stop:    exomonad stop --run-id <run> --session exomonad-<project>
```

The host is a restart-bounded per-run systemd user service. The session has a
`Host` window following that service, a `Compiler` window
running the Haskell compile service, and a `exomonad-root` window with the root
agent's client. Give the root a task there. Each child agent gets a window of its own
when it starts.

| Option | Effect |
|---|---|
| `--session NAME` | name the tmux session |
| `--no-attach` | start, print the connection details, and return |
| `--recreate` | replace an existing session of that name |
| `--model`, `--effort` | override the workspace config for this run |
| `--workspace PATH` | start in another project |

## While it runs

**Drive it from a terminal.** `exomonad proxy <session> cell.hs` submits one Haskell
cell to the running session as a resident operator with its own persistent
bindings; `-` reads the cell from standard input and `--actors` prints the actor
graph. See [the operator interface](operator-http.md).

**Change code without restarting.** Workspace modules are captured at launch.
After an agent edits one, `reloadSource` typechecks the edited source and
publishes it for later cells, or refuses and leaves the session as it was.
`reload_agent_spec` rebuilds the agent's own typed tool record from the
published source, so later tool calls use the edited body. A reload that would
change a tool's name, description or argument types is refused with the
difference, and takes effect at the agent's next incarnation. Prompts, and the
Haskell library Tidepool ships, change only with a new run.

**See what is live.** The agent's `status` tool, with `view: "detailed"`, shows
which spec is installed and from which file and revision, retained command jobs,
bindings, and whether the
source on disk has drifted from what is loaded.

**Read the trace.** `.exomonad/logs/<run>.jsonl` is a structured trace: run, actor,
tool call, cell, unit. By default it includes cell source and tool results in
full. `EXOMONAD_TRACE=info` leaves content out. The directory is ignored by Git.

## Stopping and cleaning up

Use `exomonad stop --run-id <run> --session <session>` for intentional shutdown;
stopping only tmux leaves the supervised host running. A host failure restarts
with backoff and a finite retry limit. Recovery reopens the same run under its
exclusive incarnation lock, stops every predecessor application whose exact
supervisor identity can be proven, resumes the recorded conversation, reloads
the last accepted source, and sends a recovery notice before new work. Child
conversations resume independently when their accepted source, process stop,
lineage, launch policy, and optional worktree custody all verify. Their logical
actor IDs remain stable and their incarnations advance. Actors whose evidence
cannot be verified remain visibly unavailable. Run status records recovered
predecessor and successor identities, lost live state, and a bounded resource
service snapshot with retained allocations and cleanup failures. It also
samples run-directory storage through a bounded walk and reports when that
sample was truncated.
Recovery requires the lifecycle v2 journal and its durable creation marker.
Runs with only the older `actor-lifecycle.v1.jsonl`, or without hosted-operation
ownership journals, remain inspectable but are not resumed automatically.
There is no automatic journal migration; renaming an old journal does not make
its ownership evidence sufficient.
Live Haskell values, requests, watches, and bindings are reported lost rather
than reconstructed. Unresolved tool calls are not replayed automatically.

If the first host startup fails before publishing its root binding and lifecycle
evidence, restarting that run may remain unavailable. Start a fresh run and
retain the failed run's artifacts for inspection and confirmed cleanup. Host
incarnations are never rolled back to bypass missing ownership evidence.

A stopped run's live heap and handles are gone. Durable command ownership,
cleanup failures, accepted source, and process evidence remain until retirement
is confirmed. The resource service reconciles its journal with delegated
cgroups before granting new work.
`exomonad cleanup` inspects a stopped run's build storage and never touches source
or Git state. `exomonad run-map` reads a run's recorded artifacts without starting
or attaching to anything.

## Where to go next

- [The workspace package in depth](../examples/workspace/README.md), with a
  fuller example: orchestration modules, prompts, recipe checks.
- The skills, which are the manual an agent reads:
  [Jev](../examples/workspace/.exomonad/skills/exomonad-jev/SKILL.md),
  [the agent spec](../examples/workspace/.exomonad/skills/exomonad-agent-spec/SKILL.md),
  [the workbench](../examples/workspace/.exomonad/skills/exomonad-workbench/SKILL.md),
  [delegating](../examples/workspace/.exomonad/skills/exomonad-unfold/SKILL.md).
- [The glossary](../../docs/GLOSSARY.md), for what the words mean here.
