import qualified Tidepool.Actor as Actor
data Handoff result = ComponentFinished (WorkEvent Delivery) result | HandoffSnapshot ([WorkEvent Delivery] -> result)
let parentDefinition = (Actor.stateful "parent-handoff" Actor.ReadOnly (\events message -> case message of
      ComponentFinished event answer -> pure (answer, events ++ [event])
      HandoffSnapshot answer -> pure (answer events, events)) :: Actor.ActorDefinition [WorkEvent Delivery] Handoff [WorkEvent Delivery])
parent <- Actor.startActor parentDefinition []
let forwardFinal = (\event -> case event of
      WorkFinished _ _ -> do { Actor.cast parent (ComponentFinished event ()); pure Nothing }
      _ -> pure Nothing) :: WorkSink Delivery
handoff <- followWork [("left", forkedResponse left, leftProgress), ("right", forkedResponse right, rightProgress)] forwardFinal
