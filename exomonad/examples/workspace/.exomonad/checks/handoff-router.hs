import qualified Tidepool.Actor as Actor
data Handoff mode = Handoff { handoffState :: mode :- State [WorkEvent Delivery], componentFinished :: mode :- Call (WorkEvent Delivery) NoReply, handoffSnapshot :: mode :- Call () (R.Reply [WorkEvent Delivery]) }
let parentDefinition = coordinationActor "parent-handoff" Handoff
      { handoffState = []
      , componentFinished = \event -> modify' (++ [event])
      , handoffSnapshot = \() -> get
      }
parent <- R.start parentDefinition
let forwardFinal = (WorkSink $ \_ event -> case event of
      WorkFinished _ _ -> do { R.send (componentFinished (R.client parent)) event; pure noWorkDelivery }
      _ -> pure noWorkDelivery) :: WorkSink Delivery
Right handoff <- followWork [("left", left, leftProgress), ("right", right, rightProgress)] forwardFinal
