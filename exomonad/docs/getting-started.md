# Getting started with Exomonad

Two commands do two different jobs. `exomonad new` creates a project workspace
with a pinned shared-source submodule. `exomonad init` starts a run in a project
that has one. Everything else on this page is what happens around those two.

## What you need

Linux, with Nix 2.27 or newer, systemd user services on cgroup v2, Bubblewrap and
tmux. The embedded backend serves the root conversation in a browser and calls
the provider through the harness. Its current authentication bridge reads an
existing Codex credential file; authenticate that account before model work.
The Codex compatibility backend uses the pinned Tidepool fork of the client.
For Jev, put a TypeSafe key in `TYPESAFE_API_KEY` before launching.

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

For Codex compatibility, the matched package uses its `local` Cargo profile:
no LTO, no debug information, and unoptimized code with release runtime semantics. This reduces
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

The resident compiler is owned by the run's Compiler tmux window. Its worker
count and per-worker RSS rotation ceiling can be set in `.exomonad/config.toml`:

```toml
[compiler]
workers = 2
rss_ceiling_mb = 10240
```

Both values must be positive. Omitted settings retain one worker and a 7168 MiB
ceiling. The example is the approved swarm-01 measurement configuration; each
worker may reach 10 GiB, and the enclosing systemd slice remains the aggregate
memory limit. The compiler trace records daemon epoch, daemon/worker PIDs,
queue wait, service time and worker RSS so rotation is visible in measurements.

Child agents are launched from committed checkouts. Commit the package before
you ask an agent to delegate.

## Agent specification

The conventional `AgentSpec` module exports `agentSpec`; `[haskell] spec` can
select another module. When neither exists, the default spec is empty. The obsolete
`[haskell] tools` key is rejected: install the typed record through `installSpec`
and select that spec. Typed tools and actor-mailbox records are different DSLs;
malformed tool arguments return typed dispatch errors before the body runs.

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

Select the embedded backend in the project's `.exomonad/config.toml`:

```toml
[launch]
backend = "embedded"
```

For native ChatGPT plan authentication, sign in with a separate Exomonad record:

```bash
exomonad auth login --credential-file /absolute/private/path/chatgpt.json
```

Open the displayed authorization URL in a browser on the same host. The callback
listens on `127.0.0.1:1455/auth/callback`; `--port` selects another port. On a
self-hosted VM, the [official VM guidance](https://developers.openai.com/siwc/token-sharing-open-source/self-hosted-vms)
describes signing in locally and securely transferring the selected credentials
while preserving the VM's separate host ID. An SSH loopback port forward is a
possible alternative for this CLI, but has not been qualified here.

Select the native public Responses route explicitly:

```toml
[launch.embedded]
provider = "chatgpt_plan"
credential_file = "/absolute/private/path/chatgpt.json"
```

This uses `https://api.openai.com/v1/responses`, with full request history,
`store = false`, and `stream = true`. The credential record belongs to Exomonad;
existing Codex credentials do not grant this flow's ChatGPT plan permission.
Async declarations are preserved on the public route; account-specific live
async behavior still needs an authenticated trial.

Configure `[launch.embedded]` with `listen`, `credential_file`, and the model's
`context_capacity_tokens`. The listener must
use loopback or a local Tailscale address. File paths must be absolute;
`asset_root` must contain the browser's `index.html`, or be supplied by the
package through `EXOMONAD_EMBEDDED_ASSET_ROOT`. Provider authentication is
independent of browser access. `provider = "codex"` retains the read-only Codex
credential bridge; the old `codex_auth_file` spelling remains an alias for
`credential_file`. Omitting `provider` selects that existing bridge.
For automatic browser access from your Tailscale devices, pin the browser URL
and configure an explicit numeric Tailscale user allowlist:

```toml
[launch.embedded]
public_origin = "http://swarm-01:8080"

[launch.embedded.browser_auth]
mode = "tailscale"
allowed_user_ids = [123456789]
```

Replace the URL with your listener's browser URL and the ID with your Tailscale
user ID. The pinned URL prevents another website from using browser access
through DNS rebinding. This mode requires a listener
address assigned to `tailscale0`, verifies the actual remote device through the
local Tailscale daemon, and rejects tagged, shared, and local-self peers. It does
not use `session_secret_file`. Access is rechecked during live browser streams.
For secret-based access, omit `browser_auth` and supply an absolute
`session_secret_file`; the browser signs in with that file's contents.
Model and effort selection remain in `[defaults]` and the
workspace's `[models]` aliases. Backend selection is a config setting; `init`
has no `--backend` flag.

```bash
cd /path/to/your/project
exomonad init
```

The first live run is operator-driven: start it when ready, open the browser
listener reported by the host, authenticate using the configured mode, and submit
the root task there. Embedded implementation and focused checks do not by
themselves establish full engine acceptance; see the
[current delivery status](../../plans/README.md). Selecting this backend for a
workspace does not change the generated default or publish a release.

`exomonad init` scaffolds nothing. With no workspace it stops and says to run
`exomonad new`. With one, it captures the workspace, starts a tmux session named
after the project, and prints where things are:

```
log:     …/.exomonad/logs/<run>.jsonl
status:  ~/.local/state/tidepool/exomonad/runs/<run>/status.json
attach:  tmux attach -t exomonad-<project>
stop:    exomonad stop --run-id <run> --session exomonad-<project>
```

`XDG_STATE_HOME` replaces `~/.local/state` when set. Run journals, retained
executables and managed worktrees live under this durable state root; deleting
`~/.cache` does not delete a new run. Older runs remain at their recorded cache
paths. Use their explicit `--run-root` for host recovery or cleanup. If a
workspace still has managed worktrees in the old cache, `exomonad init`
refuses to start a separate state-root run until that work is inspected and
retired; it does not move or delete the old tree automatically. `exomonad proxy`
discovers live sessions in both roots and refuses an ambiguous session name.

The host is a restart-bounded per-run systemd user service. The tmux session has
a `Host` window following that service and a `Compiler` window running the
Haskell compile service. Embedded root and child conversations appear in the
browser; they do not require Codex client processes. With the Codex compatibility
backend, `exomonad-root` and child windows hold the clients instead.

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
The run owner can edit the run workspace and call `reloadSource` to typecheck
and publish its authored modules for later cells. Child actors use that same
run tooling; editing a historical module in a child checkout does not reload
it. The run owner can call `reload_agent_spec` to rebuild its own typed tool
record from published source. A reload that would
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
with backoff and a finite retry limit. Embedded host recovery reopens the same
run under its exclusive incarnation lock and follows the exact root startup
chain. It verifies the accepted source and compiler-issued bootstrap identity,
then observes the public manifest owner and Store owner independently. The new
root retains its installed executable entry without evaluating it until the
manifest, Store binding, and ApplicationBound journal event are durable. Only
then can the host attach the retained conversation and accept new work.
Actors whose evidence cannot be verified remain visibly unavailable. Run
status records recovered
predecessor and successor identities, lost live state, and a bounded resource
service snapshot with retained allocations and cleanup failures. It also
samples run-directory storage through a bounded walk and reports when that
sample was truncated.
Recovery requires version 5 records in `actor-lifecycle.v2.jsonl`, including
its durable creation marker and atomic root startup intent. Older formats,
including version 4 raw compiled-program hashes, are refused without rewriting
their evidence. There is no automatic journal migration; renaming an old
journal does not make its ownership evidence sufficient. Fresh launches of the
deprecated Codex backend remain supported, but its recovery is refused because
it does not record the required startup intent and bootstrap identity.
Live Haskell values, requests, watches, and bindings are reported lost rather
than reconstructed. Unresolved tool calls are not replayed automatically.

An interrupted embedded startup can advance through a fresh root admission
when its exact startup chain and independently observed owners remain valid.
Missing or changed evidence leaves the run unavailable. Retain the failed
run's artifacts for inspection and confirmed cleanup. Host incarnations are
never rolled back to bypass missing ownership evidence.

A stopped run's live heap and handles are gone. Durable command ownership,
cleanup failures, accepted source, and process evidence remain until retirement
is confirmed. The resource service reconciles its journal with delegated
cgroups before granting new work.

Source imports copy tracked working files (including locally edited or ignored
tracked files) and ordinary untracked files. Ignored files are omitted.
Untracked nested repositories require an explicit Git ignore rule or source
exclusion so that their working files are never silently dropped. Tracked
submodules are selected from their own Git
index. Source imports preserve symlinks as links and preserve hard links and
sparse files where supported. Before copying, the host budgets selected logical
file size or allocated blocks, whichever is larger, and requires free space for
that budget plus a reserve. The defaults are 8 GiB per import and 16 GiB free
after import; `[launch.source]` can set `max_import_bytes` and `min_free_bytes`
for a workspace. These are conservative admission policies, not a disk quota
for later actor writes. The host reports the selected budget, free space, and
copy method when admitting or refusing an import. Large build artifacts should
live outside source or in the separately mounted build directory.
`exomonad cleanup` inspects a stopped run's build storage and never touches source
or Git state. `exomonad run-map` reads a run's recorded artifacts without starting
or attaching to anything.

Retired managed worktrees retain sealed source layers rather than eagerly copying
the whole checkout. The [worktree registry](../../exomonad/worktree/src/registry.rs)
records that custody; [retirement and restoration](../../exomonad/worktree/src/create.rs)
preserve the layers and materialize them on demand. A retained checkout path is
not an ordinary readable working tree until restoration finishes. Inspect a
child's committed work through the shared Git namespace and typed observation.

[Copy admission](../../exomonad/node/src/copy_admission.rs) serializes cooperating
copies on a filesystem; [source import](../../bridge/facade/src/actor_host/workspace.rs),
[compaction](../../bridge/facade/src/actor_host/overlay_resource.rs) and worktree
restoration acquire it before materializing files. It does not bound later writes
or total durable retention. Before claiming bounded storage across repeated runs,
retain a repeated fork/retire/cleanup inventory and restart restoration evidence,
including staged and unstaged changes, submodules and untracked work. ENOSPC
checks must exercise import, publication, retirement and journal writes while
preserving the previous authoritative state. Reclaim only after confirming that
live writers, mounts and retained readers no longer depend on the resource.

`exomonad run-map <run-dir> --perfetto` exports recorded host trace metadata for
Perfetto. Use `--since 15m`, `--actor ID@INCARNATION`, `--execution ID`, or
`--call-id ID` to bound the timeline. `--from-unix-ms` and `--until-unix-ms`
accept UTC Unix milliseconds. Selector flags filter timeline events while the
actor inventory and review retain their normal scope. The export includes
trace source file and line references plus counts for time cutoff, selectors,
the timeline limit, and unclassified records. It uses fixed event labels and
exports metadata only; model input, output, errors, and arbitrary log messages
are not included. It draws duration spans only for known tracing close records;
other durations stay attached to instant events. It does not make missing
identifiers into causal links. Usage remains unknown until a harness-owned
evidence export is available.

## Where to go next

- [The workspace package in depth](../examples/workspace/README.md), with a
  fuller example: orchestration modules, prompts, recipe checks.
- The skills, which are the manual an agent reads:
  [Jev](../examples/workspace/.exomonad/skills/exomonad-jev/SKILL.md),
  [the agent spec](../examples/workspace/.exomonad/skills/exomonad-agent-spec/SKILL.md),
  [the workbench](../examples/workspace/.exomonad/skills/exomonad-workbench/SKILL.md),
  [delegating](../examples/workspace/.exomonad/skills/exomonad-unfold/SKILL.md).
- [The glossary](../../docs/GLOSSARY.md), for what the words mean here.
