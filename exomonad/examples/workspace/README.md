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

`Project.Watchdog` provides monitor logic for an after-tool hook a parent
installs on a child. It can add advice to the child's result or send an
escalation to the parent; it does not present shell or lookup results.

The [workspace skills](.exomonad/skills/) describe the current Exomonad
interfaces. The [check modules](.exomonad/Project/Checks.hs) and
[`config.toml`](.exomonad/config.toml) define the checks run with `--recipes`.
