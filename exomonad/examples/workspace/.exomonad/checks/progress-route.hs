import qualified Tidepool.Actor as Actor
import Tidepool.Effects.Row (knownEffects)
import qualified Data.Text as Text
data RoutingCount result = RoutingCount Int (Int -> result)
let countDefinition = (Actor.stateful "routing-effects" (Actor.Selected (knownEffects @'[])) (\n (RoutingCount delta reply) -> pure (reply n, n + delta)) :: Actor.ActorDefinition Int RoutingCount Int)
wakes <- Actor.startActor countDefinition 0
Right forwarding <- traverse R.start (workDefinition [("producer", R.settlement producer, R.progress updates)] (WorkSink $ \_ event -> case workMessage id event of { Nothing -> pure noWorkDelivery; Just _ -> do { Actor.cast wakes (RoutingCount 1 (const ())); pure noWorkDelivery } }))
