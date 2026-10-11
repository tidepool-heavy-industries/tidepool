import qualified Tidepool.Effects.Core as Effects

_ <- do
  Right response <- send (Effects.JevAskWith "hold ancestor output until successor preparation")
  True <- pure (response == "released")
  display (40 + 2 :: Int)
