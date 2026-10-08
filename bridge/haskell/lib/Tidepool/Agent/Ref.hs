-- | Opaque addresses of admitted agents. References are issued by the runtime
-- or returned by spawn; their identities can be inspected without minting one.
module Tidepool.Agent.Ref
  ( AgentRef
  , agentIdentity
  , agentAddressText
  , agentBoundWorktree
  ) where

import Tidepool.Agent.Ref.Internal
  ( AgentRef, agentIdentity, agentAddressText, agentBoundWorktree )
