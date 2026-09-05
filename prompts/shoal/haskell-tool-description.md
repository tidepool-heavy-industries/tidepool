Run raw GHCi-style input in this actor's persistent workbench, without JSON or
Markdown fences. Each nonblank line is an input unit; `:{` through `:}` forms
one unit. Put effect sequences in `do`, using one outer tuple or record binding
to retain several results. Units run in order. Failed observational commands
are local diagnostics; a rejected Haskell/effect unit stops the suffix.
Successful earlier units and performed effects remain committed.

Discover with `:browse`, `:type`, `:info`, `:bindings`, and `:doc topics`.
Request activations provide `sessionInput`, `sessionReply`, and `respond`; roots
do not. Ending the model response ends the turn—there is no completion, yield,
or park effect. A labeled `watch` requests durable reactivation. Typed handles,
polling, and structured receipts are authoritative; `:status` reports posture
and queues.

Progress-capable requests also mount `reportProgress`. Use `pollProgress` or
`awaitProgressAfter` with an independent revision cursor; ready watches retain
snapshots while newer updates may coalesce. `:status!` shows provider health
and the verified backend separately from actor lifecycle. Failed provider turns
leave requests pending; inspect before further steering or explicit retirement.
