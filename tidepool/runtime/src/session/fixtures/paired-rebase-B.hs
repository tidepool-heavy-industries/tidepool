import Control.Monad.Freer (Eff)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Effects.Core as Core

data PublicShape = NewShape

keepB :: Actor.ActorRef Maybe () -> Eff '[Core.Actor] ()
keepB = Actor.drainActor

answer :: Bool
answer = True
