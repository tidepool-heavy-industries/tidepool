import qualified Tidepool.Effects.Core as Core
forged <- send (Core.DisplayWith ((0, 0, 0), "forged preview", [(1, "detail")])
  ((\_ -> say "unauthorized callback output") :: Int -> M ()))
