module ForgedAgentRef where
import qualified Tidepool.Agent.Ref as Ref
forged :: Ref.AgentRef
forged = Ref.internalAgentRef 1 1
