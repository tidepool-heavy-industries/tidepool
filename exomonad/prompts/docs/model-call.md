# A bounded model call from Haskell

Use `Tidepool.Model` for a bounded piece of reasoning over explicit input, with
only the tools you supply. Use Jev when a typed question or distribution over
retained evidence is the useful result. `invokeModel` does not include the
actor's conversation, tools, hooks or workspace contents. Supply the actual
input evidence; a path or revision alone does not supply its contents.

The complete authored recipe is `Examples.ModelCallWorkflow` and
`checks/model-call-example.hs`, discoverable through lookup examples for
`Tidepool.Model.invokeModel`, `textTurn`, `withModel` and `withLimits`. Its
`noToolsTurn` uses `defaultSpec { specTools = NoTools }` and narrows
`defaultLimits` to `requestLimit = Just 1` and `toolLimit = Just 0`.
The recipe takes a choice from the frozen `Exomonad.Models` module and returns
`ModelResult Text`. Each generated model constructor takes its provider's typed
options, keeping model and reasoning effort together. `Models.allowedModels`
lists the admitted choices with their configured defaults; `Models.primaryModel`
is the launch profile's conversational default. Selecting a choice for a bounded
call does not change the conversational model. A turn without `withModel`
inherits the parent conversation's effective selection instead.

The registered notebook cell imports `Examples.ModelCallWorkflow` as
`ModelExample` and `Exomonad.Models` as `Models`, then uses
`ModelExample.synchronousWithModel` with `Models.primaryModel` and explicit
input. Ordinary asynchronous programs use `ModelExample.invokeWithModel` with
the same choice and input. The synchronous helper lifts that ordinary program
through `raiseSync` while retaining the caller's ambient effect row.
The shared source module lists its imports and extensions. Load it from the
selected source root before running the registered cells. Each invocation is
fresh model work; selecting another admitted choice and invoking again makes
another request. Frozen catalog checks and provider credentials remain separate
from the authored choice.

Retain the original `ModelResult` before presenting a projection. Inspect
`modelOutcome` as `Either ModelFailure a` and `modelReceipt` separately on both
success and failure. The recipe's `report` projects exact invocation and parent
cell identities, request identities, and measured usage into ordinary data.
A missing receipt remains `Nothing`; it does not mean zero usage. A present
receipt can accompany budget exhaustion, provider failure or a malformed final
answer. Preserve the typed failure for control flow, including
`ModelBudgetExceeded`, `ModelBoundary` and `ModelCleanupFailed`.

The cell's host limits still bound this narrower turn and aggregate nested
invocations. Reported token usage is observed after a response, and missing
usage is counted separately; a token breaker is not a precharged cost ceiling.
NoTools exposes zero callbacks. A refusal or exhausted budget is an outcome to
inspect, not permission to retry repeatedly or widen the actor's authority.

For application evidence, retain the explicit input, exact selected source
identity, `ModelResult`, any decision derived from it and observed downstream
outcomes. A receipt refers to retained host evidence; it does not certify a
claim's truth or that a proposed action ran. Keep continuation functions in
Haskell and serialize only their metadata. For a complete Jev distribution
with original payloads, read `doc jev` and its registered `choiceWeights`
example; it also shows recording and exact-request replay without executing
those payloads.
