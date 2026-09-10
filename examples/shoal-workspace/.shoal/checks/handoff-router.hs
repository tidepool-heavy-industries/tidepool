import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
data Handoff mode = Handoff { handoffState :: mode :- State [WorkEvent Delivery], componentFinished :: mode :- Call (WorkEvent Delivery) NoReply, handoffSnapshot :: mode :- Call () (R.Reply [WorkEvent Delivery]) } deriving Generic
let parentDefinition = coordinationActor "parent-handoff" Handoff
      { handoffState = []
      , componentFinished = \event -> modify' (++ [event])
      , handoffSnapshot = \() -> get
      }
parent <- R.start parentDefinition
let forwardFinal = (\event -> case event of
      WorkFinished _ _ -> do { R.send (componentFinished (R.client parent)) event; pure Nothing }
      _ -> pure Nothing) :: WorkSink Delivery
handoff <- followWork [("left", forkedResponse left, leftProgress), ("right", forkedResponse right, rightProgress)] forwardFinal
