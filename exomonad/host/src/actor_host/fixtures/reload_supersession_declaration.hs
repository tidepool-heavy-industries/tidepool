import qualified Tidepool.Effects.Authored as Files

data StagedReloadDeclaration = StagedReloadDeclaration
reloadSource []
Files.writeFile "reload-tail-ran.txt" "unexpected trailing effect"
