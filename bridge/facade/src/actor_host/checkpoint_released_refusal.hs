import Data.Text (Text)
Just seed <- R.call (readSeed (R.client seedStore)) ()
let releasedGroup = "released" :: ForkGroupLabel
let releasedLabel = [label|released|]
result <- attemptUnfold (batch campaign releasedGroup)
  (child (withContext (fromCheckpoint seed)
    (researching @Text projectHead (assignment releasedLabel ("inspect" :: Text)))))
case result of
  Left (UnfoldCheckpointRefused ReleasedCheckpoint) -> display True
  _ -> display False
