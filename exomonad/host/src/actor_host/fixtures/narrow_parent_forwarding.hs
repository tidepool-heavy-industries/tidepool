import qualified Tidepool.Agent.Contract as A
import qualified Tidepool.Actor as Actor
import GHC.Generics (Generic)
data ForwardSink mode = ForwardSink { forwardSinkState :: mode :- State Int, acceptForwarded :: mode :- Call (Either ResponseFailure (ResponseResult Int)) NoReply, forwardCount :: mode :- Call () (R.Reply Int) } deriving Generic
data ForwardOwner mode = ForwardOwner { forwardOwnerState :: mode :- State (), beginForwarding :: mode :- Call () (R.Reply (R.Forwarding Int)) } deriving Generic
let sinkDefinition = R.definition "narrow-forward-sink" (Actor.Selected (knownEffects @'[])) ForwardSink { forwardSinkState = 0, acceptForwarded = \answer -> case answer of { Right result | responseValue result == 42 -> modify' (+1); _ -> error "unexpected forwarded settlement" }, forwardCount = \() -> get }
sink <- R.start sinkDefinition
Right forwardTarget <- spawnSubagent (FreshCtx "Return the typed integer") SameDir (defaultSpawnOptions currentModel (A.defaultWorkbenchSpec @'[Replies]))
Right forwardRequest <- request @Int forwardTarget (42 :: Int) defaultRequestOptions
let ownerDefinition = R.definition "narrow-forward-owner" (Actor.Selected (knownEffects @'[Actor, AgentLaunch])) ForwardOwner { forwardOwnerState = (), beginForwarding = \() -> R.forwardResult forwardRequest (acceptForwarded (R.client sink)) }
forwardOwner <- R.start ownerDefinition
forwarding <- R.call (beginForwarding (R.client forwardOwner)) ()

