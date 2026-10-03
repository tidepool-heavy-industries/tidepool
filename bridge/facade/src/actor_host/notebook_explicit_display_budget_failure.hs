import qualified Data.Text as Text
shown <- do
  say (Text.replicate 56000 "p")
  first <- display (Text.replicate 5000 "v")
  second <- display ("second" :: Text)
  pure (first, second)
error "deliberate failure after displays" :: M ()
