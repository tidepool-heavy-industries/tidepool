import qualified Tidepool.Actor as Actor
data RoutingCount result = RoutingCount Int (Int -> result)
let countDefinition = (Actor.stateful "routing-effects" Actor.ReadOnly (\n (RoutingCount delta reply) -> pure (reply n, n + delta)) :: Actor.ActorDefinition Int RoutingCount Int)
wakes <- Actor.startActor countDefinition 0
collection <- followAttentionSources [("left", leftProgress), ("right", rightProgress)] (\_ -> Actor.cast wakes (RoutingCount 1 (const ())))
