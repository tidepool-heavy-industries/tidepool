import qualified Tidepool.Effects.Core as PreflightEffects
neverPublished <- do
  send (PreflightEffects.NotifyWith (TARGET_ID, TARGET_INCARNATION) "whole-cell-preflight-sentinel")
  pure (41 :: Int)
pure (True :: Int)
