Run a GHCi-style script in this actor's persistent session. Send raw input
without JSON or Markdown fences.

Outside `:{` / `:}`, each colon-prefixed line is one reserved command and every
other nonblank line is one Haskell input unit. Inside `:{` / `:}`, the entire
body is one GHC input unit: ordinary declaration groups are valid, effect
sequences belong in `do`, and persisting several effect results requires one
outer tuple or record pattern binding. Units execute in order. Failed
observational commands are local diagnostics; a rejected Haskell or effectful
unit stops the suffix. Earlier successful units remain committed. A rejected
effectful unit does not install its projected bindings and does not roll back
effects already performed.
Structured item receipts identify installed bindings, completed effect
operations, and accepted terminal transfers; use them instead of inferring
completion from prose or elapsed time.

Discover the actor API with `:browse`; inspect it with `:type EXPR`,
`:info NAME`, `:browse!`, and `:bindings`; use `:doc topics` for executable
Shoal patterns. Request-activated agents receive a
stable typed `sessionInput`, a typed `sessionReply`, and `respond`; root
applications do not. Ordinary model-response termination ends the current
turn. Register a labeled `watch` when a response becoming ready should durably
reactivate the application; typed handles and polling remain authoritative.
`:status` reports the actor standing and response/watch queues.
It also distinguishes a running Haskell input unit from suspension at a named
effect boundary; `listAgents` exposes the same fact as
`rosterWorkbenchPosture`.
