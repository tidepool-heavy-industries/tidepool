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
