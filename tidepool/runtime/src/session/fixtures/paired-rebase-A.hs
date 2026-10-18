import Control.Monad.Freer (Eff)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Effects.Core as Core

data PublicShape = OldShape

keepA :: Actor.ActorRef Maybe () -> Eff '[Core.Actor] ()
keepA = Actor.drainActor

answer :: Int
answer = 41
