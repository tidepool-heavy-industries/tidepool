# Lookup corpus migration audit

Audit command:

```bash
rg -n '(^|[^A-Za-z]):(type|info)' \
  prompts/shoal examples/shoal-workspace/.shoal/checks \
  tidepool/src/actor_host tidepool/tests tidepool-actor/tests \
  --glob '*.md' --glob '*.hs'
```

The current exact hits and intended migrations are:

| Current source | Current shape | Stable replacement |
|---|---|---|
| `prompts/shoal/haskell-tool-description.md` | `:type`, `:info` discovery | Dedicated `lookup` example using a query batch |
| `prompts/shoal/haskell-tool-instructions.md` | `:type`, `:info` cold tail | “Use `lookup` for names and `::` type queries” |
| `prompts/shoal/base.md` | `:type`/`:info` alongside bindings | `lookup`; runtime facts move to `status` |
| `prompts/shoal/worktree-agent.md` | `:type respond` | `lookup {"queries":["respond"]}` concept, rendered in the actual hosted syntax chosen by the corpus owner |
| `prompts/shoal/docs/request.md` | truncated result gives `:info` cue | result names a `lookup` query |
| `prompts/shoal/docs/workbench.md` | `:type judge`, `:info fmt`, `:type [fmt|…|]` | one coherent lookup batch followed by a cell using the answer |
| `tidepool/src/actor_host/roster_observe.hs` | standalone `:info AgentRosterEntry` | lookup fixture separated from the following notebook cell |
| `tidepool/src/actor_host/command_output_ux.hs` | standalone `:info Cmd.RunResult` | lookup fixture separated from the command cell |

No `:type` or `:info` examples were found under
`examples/shoal-workspace/.shoal/checks`, `tidepool/tests`, or
`tidepool-actor/tests` at this source.

The corpus owner must rerun the audit after its rewrite and also search
`:browse`, `:doc`, `:bindings`, and status-family commands because their
destinations differ. This file maps only the lookup migration and does not
authorize edits to frozen `.shoal` resources in the current swarm.
