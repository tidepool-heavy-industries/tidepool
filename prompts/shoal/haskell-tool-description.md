Run GHCi-style input in this actor's persistent workbench. Each nonblank line is
one unit; `:{` through `:}` forms one unit. Sequence effects in `do`; use one
tuple or record binding to retain several results. Units run in order. Failed
observations are local; a rejected Haskell/effect unit stops the suffix. Prior
units and effects remain committed.

Discover with `:browse`, `:type`, `:info`, `:bindings`, and `:doc topics`.
Request activations expose `sessionInput`, `sessionReply`, and `respond`.
Ending the model response ends the turn; a labeled `watch` reactivates it.
Typed handles and receipts are authoritative; `:status` reports queues.

Progress requests expose `reportProgress`. Poll via `pollProgress` or
`awaitProgressAfter` with a separate cursor. Ready watches retain snapshots
while updates may coalesce. `:status!` separates provider health from actor
lifecycle. Failed provider turns leave requests pending; inspect before steering
or retirement.
