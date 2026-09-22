module AwaitSettledDependencies where

import Tidepool.Agent.Ref (internalAgentRef)
import Tidepool.Agent.Reply.Internal
  ( RequestId (..)
  , Response
  , newRequestHandles
  )
import Tidepool.Agent.Watch.Internal
  ( Await (..)
  , AwaitDependency (..)
  , awaitSettled
  )

sampleResponse :: Response Int
sampleResponse = response
  where
    (response, _) = newRequestHandles () (RequestId 17) (internalAgentRef 23 29)

awaitSettledDependencies :: [[(Int, Bool)]]
awaitSettledDependencies = case awaitSettled sampleResponse of
  Await dependencies _ -> map (map dependency) dependencies
  where
    dependency (AwaitDependency (RequestId request) settled) = (request, settled)
    dependency (AwaitProgress (RequestId request) _) = (request, False)
