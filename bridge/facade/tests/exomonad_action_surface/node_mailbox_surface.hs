module NodeMailboxSurface where

import Tidepool.Effects (M)
import Tidepool.Node (NodeHandle, forkNode)

result :: M (NodeHandle () () ())
result = forkNode (\_ -> pure ())
