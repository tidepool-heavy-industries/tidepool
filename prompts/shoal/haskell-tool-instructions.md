Use `tidepool_actor.haskell` as the primary actor orchestration surface. Its raw
payload is a GHCi-style script, not JSON or Markdown.

Outside `:{` / `:}`, each nonblank line is one input unit. A fenced body is one
GHC input unit: use ordinary declaration groups, put effect sequences in `do`,
and use one outer tuple or record pattern binding to persist several results.
Units run in order and preserve successful prefixes; effects are not rolled
back when a unit rejects.

Start discovery with `:browse`; use `:type`, `:info`, `:browse!`, and
`:bindings` for detail. Use native coding tools for repository work.
