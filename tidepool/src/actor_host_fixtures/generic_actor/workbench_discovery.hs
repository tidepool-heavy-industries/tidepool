:show imports
:browse
:{
actorEffectsIdentity :: Eff ActorEffects a -> Eff ActorEffects a
actorEffectsIdentity = id
:}
import qualified Data.Set as Set
:show imports
:type Set.empty
emptySet = Set.empty :: Set.Set Int
:type emptySet
