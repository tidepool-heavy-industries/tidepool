import qualified Tidepool.Actor as Actor
import qualified Data.Text as Text
data RoutingCount result = RoutingCount Int (Int -> result)
let countDefinition = (Actor.stateful "routing-effects" Actor.ReadOnly (\n (RoutingCount delta reply) -> pure (reply n, n + delta)) :: Actor.ActorDefinition Int RoutingCount Int)
wakes <- Actor.startActor countDefinition 0
let countChanges = (\event -> case workMessage id event of { Nothing -> pure Nothing; Just _ -> do { Actor.cast wakes (RoutingCount 1 (const ())); pure Nothing } }) :: WorkSink Text
collection <- followWork [("left", forkedResponse left, leftProgress), ("right", forkedResponse right, rightProgress)] countChanges
