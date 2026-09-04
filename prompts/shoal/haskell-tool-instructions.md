Use `tidepool_actor.haskell` as the primary actor orchestration surface. Its raw
payload is a GHCi-style script, not JSON or Markdown.

Outside `:{` / `:}`, each nonblank line is one input unit. A fenced body is one
GHC input unit: use ordinary declaration groups, put effect sequences in `do`,
and use one outer tuple or record pattern binding to persist several results.
Units run in order and preserve successful prefixes. A failed observational
command such as `:type` or `:info` is a local diagnostic and later independent
observations still run. A rejected Haskell/effectful unit stops the suffix;
effects already performed are not rolled back.
Item receipts expose installed bindings, effect operation IDs/dispositions,
and accepted terminal transfers. An exact transport retry returns its retained
receipt; writing the same source in a new hosted call is new intent.

Start discovery with `:browse`; use `:type`, `:info`, `:browse!`, and
`:bindings` for detail, `:recovery` after a successor starts, and `:status` for
runtime-owned actor/request state. `:status` and `rosterWorkbenchPosture`
distinguish active Haskell execution from suspension at a named effect; do not
infer either state from elapsed time.
Use `:doc topics` for short executable examples.
Request scopes mount typed `sessionInput`, `sessionReply`, and `respond`; roots
do not. Ending a model response ends the turn—there is no Haskell completion,
yield, or park operation. Requests and watches use validated readable labels;
typed handles remain authoritative. Use native coding tools for repository work.
