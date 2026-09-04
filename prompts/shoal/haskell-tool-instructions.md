Use `tidepool_actor.haskell` for typed orchestration. Send raw GHCi-style source.
Each nonblank line is one unit; `:{` / `:}` encloses one multiline unit. Use
`do` for effects and one outer tuple or record binding to retain several
results. Failed diagnostics do not block later diagnostics. A rejected
Haskell/effect unit stops the suffix without undoing successful work.

Start with `:browse`; use `:type`, `:info`, `:browse!`, `:bindings`, `:status`,
`:recovery`, and `:doc topics`. Receipts name installed bindings, effect
operation dispositions, and terminal transfers. Exact transport retries return
the retained receipt; source in a new call is new intent. Roots have no reply
binding. Requests expose `sessionInput`, `sessionReply`, and `respond`. Ending
the response ends the turn. Register a labeled `watch` for durable
reactivation; poll typed handles for truth. Use native tools for repository work.
