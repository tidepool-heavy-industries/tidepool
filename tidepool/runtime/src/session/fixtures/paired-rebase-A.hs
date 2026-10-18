import Control.Monad.Freer (Eff)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Effects.Core as Core

data PublicShape = OldShape

keepA :: Actor.ActorDefinition () Maybe () -> () -> Eff '[Core.Actor] (Actor.ActorRef Maybe ())
keepA = Actor.startActor

answer :: Int
answer = 41
