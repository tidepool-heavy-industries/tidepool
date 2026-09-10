# Follow-on: shared Codex execution with ordinary TUIs

Direction for a subsequent design pass, not applications wrap-up acceptance.
Explore one native execution server per swarm with a normal interactive Codex TUI
per actor. Reuse the existing app-server/client boundary. Preserve human steering,
approvals, hosted Haskell, exact-context forks and per-actor execution environments.

The motivation is duplicated process-wide services and retained memory across many
embedded servers. RSS sums overcount shared pages; quantify private/PSS and swapped
memory before claiming savings. Use scripted providers for idle, active, forked
and retired sessions. Conversation/history data remains per-session unless its
representation is deliberately shared.

First prove two full TUIs attached to one server, with different mounted workspaces,
correct command execution, independent input/approvals, hosted completion and one
exact-context fork. A working-directory change does not reproduce a Bubblewrap
mount namespace. Preserve existing command resource and process-scope owners.

Decide session lifetime separately from pane lifetime and server lifetime. Preserve
one execution owner per session and explicit behavior when the shared server dies.
Do not silently fall back to embedded execution for an uncertain live session.
A per-swarm server bounds the affected run; it is not a machine-wide singleton.

Defer immutable prefix sharing, history paging changes and broader backend
abstractions until evidence identifies the useful next step. The preceding wave
hands over reviewed owners and coupling findings, not speculative migration code.
