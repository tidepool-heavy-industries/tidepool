import qualified Control.Monad.Freer as Freer
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Lookup.Tools as LookupTools
_ <- do
  let primary = Core.LookupBatch [] [] "fixture-view" (Just "fixture primary unavailable") "fixture-primary"
        (Core.LookupSearchReceipt "fixture-view" Core.LookupSearchNotExecuted [] [] Core.LookupDiscoveryNotRequested)
      lookupFixture :: Core.Lookup a -> Eff effects a
      lookupFixture (Core.LookupRaw _) = pure primary
      reflectFixture :: Core.Reflect a -> Eff effects a
      reflectFixture _ = error "unavailable primary unexpectedly reflected"
  (captured, evaluation) <- Freer.interpret reflectFixture
    (Freer.interpret lookupFixture (Composed.captureThenAssess "find a supported API" (LookupTools.LookupArguments ["missing"])))
  case (LookupTools.captureSelection captured, J.result evaluation) of
    (LookupTools.SelectionSkipped LookupTools.PrimaryUnavailable, Right answer) ->
      case J.judge J.careful answer of
        Right decision | not (J.settledValue decision) -> pure ()
        _ -> error "confident insufficiency lost its distinct result"
    _ -> error "primary refusal or complete reassessment was lost"
True
