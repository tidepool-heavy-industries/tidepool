import qualified Tidepool.Effects.Authored as Files

reloadSource []
Files.writeFile "reload-tail-ran.txt" "unexpected trailing effect"
