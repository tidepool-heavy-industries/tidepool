A record actor is a small interpreter for typed calls and events. Its record
names private state, public operations, and fixed handlers. Keep one-shot
computations as ordinary Haskell functions; use a persistent actor when state or
event routing must continue across model turns. The owner can query or steer that
machine through its typed endpoints while other work proceeds.

Project-specific report and coordination types belong in authored workspace
modules. They are ordinary compositions over typed requests, progress, and
readiness, not runtime roles or a hidden batch framework. A handler should route
observable outcomes into the next transition and retain any cleanup uncertainty.
Handlers remain serialized while suspended; never await an event that requires
another handler on the same mailbox to run.

Load `exomonad-define-actors` for record definitions and typed clients. Load
`exomonad-agent-work` for request and `Await` composition.
