import qualified Tidepool.Effects.Authored as Files
Right beforeCrash <- Files.readFile "effect-count.txt"
Right () <- Files.writeFile "effect-count.txt" (beforeCrash <> "y")
sleep (seconds 60)
display True
