# Example Exomonad workspace

This tracked `.exomonad/` package is the starter source used by `exomonad new`.
It contains the agent specification, typed tools, prompts, skills, and
workspace checks. `exomonad check --workspace <path>` validates the selected
configuration and Haskell modules; add `--recipes` to run its configured
checks.

## Typed tools

[`AgentSpec.hs`](.exomonad/AgentSpec.hs) composes the records in
[`Project/Tools.hs`](.exomonad/Project/Tools.hs). The shell record uses
[`Project/Shell.hs`](.exomonad/Project/Shell.hs) to present typed command
observations and retained output. The lookup record uses
[`Project/Lookup.hs`](.exomonad/Project/Lookup.hs) to select among typed lookup
candidates with Jev. These presenters and selectors are part of their tools.
Every installed function tool chooses its model-facing text explicitly:
`presentWith id` for `Text`, `presentWith presentJson` for JSON, or
`presentWith presentDisplay` for an existing `Display` rendering.

The spec has no blanket after-tool monitor. Execution owners use the
[recursive-work procedure](.exomonad/RECURSIVE-WORK.md): scaffold a shared boundary,
admit ready children, integrate checked results and repeat locally. The installed
routing and review actors handle ordinary progress, questions and bounded repair.
Task-specific semantic decisions have explicit inputs and bounded effects.

The [workspace skills](.exomonad/skills/) describe the current Exomonad
interfaces. The [check modules](.exomonad/Project/Checks.hs) and
[`config.toml`](.exomonad/config.toml) define the checks run with `--recipes`.
