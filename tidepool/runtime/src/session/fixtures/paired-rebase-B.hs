import Control.Monad.Freer (Eff)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Effects.Core as Core

data PublicShape = NewShape

keepB :: Actor.ActorDefinition () Maybe () -> () -> Eff '[Core.Actor] (Actor.ActorRef Maybe ())
keepB = Actor.startActor

answer :: Bool
answer = True
