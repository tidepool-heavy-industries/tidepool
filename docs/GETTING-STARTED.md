# Getting started with Shoal

Two commands do two different jobs. `shoal new` writes a workspace package into
a project. `shoal init` starts a run in a project that has one. Everything else
on this page is what happens around those two.

## What you need

Linux, with Nix, systemd user services on cgroup v2, Bubblewrap and tmux. Shoal
drives a pinned Tidepool fork of the Codex client as each agent's interface, so
authenticate that client before starting model work. For Jev, put a TypeSafe
key in `TYPESAFE_API_KEY` before launching.

Agents run shell commands as you. Read
[what Shoal does not protect you from](../README.md#what-it-does-not-protect-you-from)
before pointing it at anything you care about.

### One-time machine setup: the resource slice

Every run is placed in one systemd user slice, `swarm.slice` by default, so a
swarm has an aggregate memory ceiling however many agents it grows. Give the
slice finite RAM and swap limits before the first run. Shoal checks placement
before it executes anything and records the limits it found in
`resource-budget.json` in the run directory. `[launch] systemd_slice` in the
workspace config names a different slice.

## Build

```bash
git clone https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix build .#shoal
./result/bin/shoal --help
```

There is no public binary cache yet, so the first build compiles everything,
GHC-side and Rust-side, and takes a good while.
The wrapper selects the matched extractor and client itself; it does not replace
`codex` on your `PATH`.

Working on Tidepool itself, use the development entry points instead, which
build this checkout and then run it:

```bash
just shoal-init           # build, then `shoal init` in this repository
just shoal-smoke          # the alpha smoke run, with its brief
```

## `shoal new`: write a workspace

```bash
cd /path/to/your/project
/path/to/tidepool/result/bin/shoal new
```

`shoal new` takes an empty directory, which it makes a Git repository, or an
existing repository that has no `.shoal/config.toml`. It refuses, and changes
nothing, when a workspace is already there. It writes:

| Path | What it is |
|---|---|
| `.shoal/config.toml` | models, effort, the Haskell source roots, and the jev-dsl source pin |
| `.shoal/Jev/Operators.hs` | the Jev operators, fixed to Tidepool's value type; cells reach it as `J` |
| `.shoal/AgentSpec.hs` | the agent spec: which tools the agent is offered and what runs after each tool call |
| `.shoal/Project/Tools.hs` | the tools record: the shell tools, and one starter tool of your own |
| `.shoal/skills/` | the workspace skills an agent loads by name |
| `.agents/skills/` | links into `.shoal/skills/`, which is where the client looks for skills |
| `flake.nix`, `flake.lock` | only when the project has none: one input, pinning jev-dsl |

Nothing it writes is specific to your machine, so the package belongs in Git.
A teammate who clones the project runs `shoal init` and nothing else.

Jev's operators are compiled from the jev-dsl revision the project's `flake.nix`
pins. When the project already has a `flake.nix`, `shoal new` leaves it alone
and prints the input line to add and the lock command to run. Until the pin
resolves, a session still starts, without `J`, and the agent is told so.

Child agents are launched from committed checkouts. Commit the package before
you ask an agent to delegate.

## `shoal check`: typecheck the workspace without starting anything

```bash
shoal check --workspace /path/to/your/project
```

This resolves the pinned sources and typechecks every module the config names.
It launches no agents and makes no model calls. `--recipes` also runs the
workspace's own model-free recipe checks, if it declares any.

## `shoal init`: start a run

```bash
cd /path/to/your/project
shoal init
```

`shoal init` scaffolds nothing. With no workspace it stops and says to run
`shoal new`. With one, it captures the workspace, starts a tmux session named
after the project, and prints where things are:

```
log:     …/.shoal/logs/<run>.jsonl
status:  ~/.cache/tidepool/shoal/runs/<run>/status.json
attach:  tmux attach -t shoal-<project>
stop:    tmux kill-session -t shoal-<project>
```

The session has a `Host` window running the actor host, a `Compiler` window
running the Haskell compile service, and a `shoal-root` window with the root
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

**Drive it from a terminal.** `shoal proxy <session> cell.hs` submits one Haskell
cell to the running session as a resident operator with its own persistent
bindings; `-` reads the cell from standard input and `--actors` prints the actor
graph. See [the operator interface](SHOAL-OPERATOR-HTTP.md).

**Change code without restarting.** Workspace modules are captured at launch.
After an agent edits one, `reloadSource` typechecks the edited source and
publishes it for later cells, or refuses and leaves the session as it was.
`reload_agent_spec` does that and also rebuilds the agent's own tools and
after-tool slot, so the next tool call runs the edited body. A reload that would
change a tool's name, description or argument types is refused with the
difference, and takes effect at the agent's next incarnation. Prompts, and the
Haskell library Tidepool ships, change only with a new run.

**See what is live.** The agent's `status` tool, with `view: "detailed"`, shows
which spec is installed and from which file and revision, what the after-tool
slot did on each recent call, retained command jobs, bindings, and whether the
source on disk has drifted from what is loaded.

**Read the trace.** `.shoal/logs/<run>.jsonl` is a structured trace: run, actor,
tool call, cell, unit. By default it includes cell source and tool results in
full. `SHOAL_TRACE=info` leaves content out. The directory is ignored by Git.

## Stopping and cleaning up

`tmux kill-session -t <session>` stops a run. A stopped run's live heap, jobs
and handles are gone; what survives is what was committed or written to files.
`shoal cleanup` inspects a stopped run's build storage and never touches source
or Git state. `shoal run-map` reads a run's recorded artifacts without starting
or attaching to anything.

## Where to go next

- [The workspace package in depth](../examples/shoal-workspace/README.md), with a
  fuller example: orchestration modules, prompts, recipe checks.
- The skills, which are the manual an agent reads:
  [Jev](../examples/shoal-workspace/.shoal/skills/shoal-jev/SKILL.md),
  [the agent spec](../examples/shoal-workspace/.shoal/skills/shoal-agent-spec/SKILL.md),
  [the workbench](../examples/shoal-workspace/.shoal/skills/shoal-workbench/SKILL.md),
  [delegating](../examples/shoal-workspace/.shoal/skills/shoal-unfold/SKILL.md).
- [The glossary](GLOSSARY.md), for what the words mean here.
