import GHC.Generics (Generic)
import Data.Text (Text)
import qualified Tidepool.Actor as Actor
let x = 41 :: Int
let getX = x + 1
data SeedBox mode = SeedBox
  { seedState :: mode :- State (Maybe ContextCheckpoint)
  , storeSeed :: mode :- Call ContextCheckpoint NoReply
  , readSeed :: mode :- Call () (R.Reply (Maybe ContextCheckpoint))
  } deriving Generic
let seedBox = R.definition "checkpoint-seeds" Actor.ReadOnly SeedBox
      { seedState = Nothing
      , storeSeed = \seed -> R.put (Just seed)
      , readSeed = \() -> R.get
      }
seedStore <- R.start seedBox
let campaign = "checkpoint" :: CampaignLabel
let producerGroup = "producer" :: ForkGroupLabel
let producerLabel = [label|producer|]
producer <- unfold (batch campaign producerGroup)
  (child (researching @Text projectHead (assignment producerLabel ("capture" :: Text))))
