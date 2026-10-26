import qualified Tidepool.Effects.Authored as Files

stagedReloadReceipt <- reloadSource []
Files.writeFile "reload-tail-ran.txt" "unexpected trailing effect"
