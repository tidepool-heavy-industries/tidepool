import Data.Text (Text)
Just seed <- R.call (readSeed (R.client seedStore)) ()
let x = 99 :: Int
let observerGroup = "observer" :: ForkGroupLabel
let observerLabel = [label|observer|]
observer <- unfold (batch campaign observerGroup)
  (child (withLifetime ActorOwned (withContext (fromCheckpoint seed)
    (researching @Text projectHead (assignment observerLabel ("inspect" :: Text))))))
