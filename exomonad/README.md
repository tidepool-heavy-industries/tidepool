# Exomonad

Exomonad runs persistent agent applications on Tidepool. An actor application
keeps its Codex conversation and Haskell workbench across typed requests. The
workbench expresses coordination and effects; Rust owns provider processes,
runtime authority, scheduling, resources, and managed worktrees.

## Typed tools

An agent specification supplies a typed record of tools. Each tool's Haskell
input and output types define the values its body receives and returns. Tool
implementations can compose existing effects without first flattening their
results into a text or JSON protocol.

Presentation policies plug into the relevant tool directly. The example
workspace's [Shell presenter](examples/workspace/.exomonad/Project/Shell.hs)
receives typed command observations and retained output; its
[Lookup selector](examples/workspace/.exomonad/Project/Lookup.hs) receives
typed lookup results and candidates. These are the examples for presenting or
selecting tool results inside the tool itself.

## Workspaces and operation

A workspace's `.exomonad/` package contains configuration, Haskell modules,
prompts, and skills. `exomonad new` creates a starter package;
`exomonad check --workspace <path>` checks a selection without starting
providers; `exomonad init` starts the run. The
[getting-started guide](docs/getting-started.md) covers setup and lifecycle.

For the resident actor and request model, see the
[agent API guide](prompts/api-guide.md) and
[shipped prompt](prompts/base.md). The
[operator interface](docs/operator-http.md) describes attachment and status.
The example [workspace guide](examples/workspace/README.md) describes its
project-specific modules and checks.
