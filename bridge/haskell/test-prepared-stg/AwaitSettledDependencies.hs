module AwaitSettledDependencies where

import Tidepool.Agent.Ref.Internal (internalAgentRef)
import Tidepool.Agent.Reply.Internal
  ( RequestId (..)
  , Request
  , newRequestHandles
  )
import Tidepool.Agent.Watch.Internal qualified as Watch

sampleRequest :: Request Int
sampleRequest = request
  where
    (request, _) = newRequestHandles (RequestId 17) (internalAgentRef 23 29)

awaitSettledDependencies :: [[(Int, Bool)]]
awaitSettledDependencies = case Watch.settledResponse sampleRequest of
  -- This projection reads the compiler-issued plan; it does not run `await`.
  Watch.Await (Watch.AwaitPlan
      [Watch.LeafNode (Watch.AwaitDependency (RequestId request) settled)] 0) _ ->
    [[(request, settled)]]
  _ -> error "settled response did not produce its single request dependency"
