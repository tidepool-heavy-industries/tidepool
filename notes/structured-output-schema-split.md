# Structured output: two schema renderers, one parser

Lifted from `plans/repo-tools/framework.md` before that plan's deletion (work
now folded into the tool framework itself; this is the one idea worth
keeping as a standing design note).

**The split.** Two classes derive a JSON Schema from the same generic `Rep`
walk: `McpSchema` (tool I/O — `tools/list` + parsing) and `LlmSchema`
(structured-output calls to an LLM). They're separate classes, not a mode
flag, because their targets diverge and the boundary belongs in the type:
a tool arg derives `McpSchema`, an LLM result derives `LlmSchema`, and a
type can't be rendered for the wrong boundary by accident.

**The by-boundary rules:**
- `Maybe` field: `McpSchema` renders it as omit-if-absent (not required);
  `LlmSchema` renders it as nullable-but-present (strict-mode structured
  output requires every key present, `null` standing in for absence).
- Sum types: `McpSchema` renders `oneOf` (aeson `TaggedObject`, matches the
  real wire `ToJSON`); `LlmSchema` prefers a string `enum` for nullary sums,
  since payload-carrying `oneOf` is unevenly supported under strict mode.

**One shared parser.** Both boundaries are read back by the same
hand-rolled `FromValue` — it must treat a `Maybe` field as `Nothing` on
*both* absent (MCP shape) and explicit `null` (LLM shape), so one parser
serves both renderers without a boundary-specific branch.

**Why this is the reusable part:** the type-level split (two classes over
one `Rep` walk, one parser reconciling both absence conventions) is the
non-obvious piece — it's what stops a future author from reaching for a
single "flexible" schema renderer with a mode flag, which would smear the
boundary back into runtime logic instead of the type.
