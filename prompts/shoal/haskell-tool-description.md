Run GHCi-style input in this actor's persistent workbench. Each nonblank line is
one unit; `:{` through `:}` forms one unit. Sequence effects in `do`; use one
tuple or record binding to retain several results. Units run in order. Failed
observations are local; a rejected Haskell/effect unit stops the suffix. Prior
units and effects remain committed.

Use `:doc topics`, `:type`, `:info`, or `:bindings` to resolve missing context.
On watch wake, poll the retained handle; skip repeated orientation.
Request activations expose `sessionInput`, `sessionReply`, and `respond`.
Ending the model response ends the turn; watches and normal steering can wake it.
Typed handles and receipts are authoritative; `:status` reports queues.

Progress requests expose `reportProgress`. Use typed source actors for ongoing
routing without rearming. `pollProgress`/finite watches observe snapshots.
`:status!` shows provider health. Failed provider turns leave requests pending;
inspect before steering or retirement.
