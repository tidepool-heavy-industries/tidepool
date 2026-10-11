import qualified Tidepool.Actor as Actor
import Tidepool.Effects.Row (knownEffects)
import qualified Data.Text as Text
data RoutingCount result = RoutingCount Int (Int -> result)
let countDefinition = (Actor.stateful "routing-effects" (Actor.Selected (knownEffects @'[])) (\n (RoutingCount delta reply) -> pure (reply n, n + delta)) :: Actor.ActorDefinition Int RoutingCount Int)
wakes <- Actor.startActor countDefinition 0
let countChanges = (WorkSink $ \_ event -> case workMessage id event of { Nothing -> pure noWorkDelivery; Just _ -> do { Actor.cast wakes (RoutingCount 1 (const ())); pure noWorkDelivery } }) :: WorkSink Text
Right collection <- traverse R.start (workDefinition [("left", R.settlement left, R.progress leftProgress), ("right", R.settlement right, R.progress rightProgress)] countChanges)
