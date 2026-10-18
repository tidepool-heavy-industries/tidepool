import qualified Tidepool.Actor as Actor
import GHC.Generics (Generic)
import Data.Text (Text)

let x = 41 :: Int
let getX = x + 1
data SeedBox mode = SeedBox
  { seedState :: mode :- State (Maybe (ContextCheckpoint OpenAI))
  , storeSeed :: mode :- Call (ContextCheckpoint OpenAI) NoReply
  , readSeed :: mode :- Call () (R.Reply (Maybe (ContextCheckpoint OpenAI)))
  } deriving Generic
let seedBox = R.definition "embedded-checkpoint-seeds" (Actor.Selected (knownEffects @'[])) SeedBox
      { seedState = Nothing
      , storeSeed = \seed -> R.put (Just seed)
      , readSeed = \() -> R.get
      }
seedStore <- R.start seedBox
pure True
