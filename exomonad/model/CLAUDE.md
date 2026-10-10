# exomonad-model — provider-neutral conversation seam

This crate owns provider-neutral roles, messages, requests, responses, usage,
accumulating transcripts, streaming observations, and the model-call trait.
It contains no backend transport implementation, actor lifecycle, Haskell
parsing or execution, retry policy, or durable provider state.

Keep transcript mutation explicit. Message position is not provider-turn
identity: several input messages and one assistant response may belong to one
turn. Opaque provider reasoning state may remain live in memory, but must not
become durable accidentally.

Response history keeps the stable runtime request identity separate from any
provider-assigned response identity. Recorded response completion is an explicit
state; absent timestamps or provider identities do not imply a response is still
in progress. Tool settlement and model-response completion remain separate facts.
