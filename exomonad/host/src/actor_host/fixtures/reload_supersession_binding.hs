import qualified Tidepool.Effects.Authored as Files

stagedReloadValue <- pure (41 :: Int)
reloadSource []
Files.writeFile "reload-tail-ran.txt" "unexpected trailing effect"
