import Control.Lens (over, view)
import qualified Tidepool.Agent.Context as C

startupPublication <- C.getInitialContext
(startupIdentity, startupAssociation, startupOriginalText, startupReference) <- case startupPublication of
  C.InitialContextPublished identity association text reference -> pure (identity, association, text, reference)
  other -> error (T.pack (show other))

startupCurrent <- C.getContext
let startupMatches = [body | C.Text (Just reference) _ body _ <- view C.contextBlocks startupCurrent, reference == startupReference]
_ <- if startupMatches == [startupOriginalText] then pure () else error "startup publication target differs from retained result"
let startupReplace body block = case block of
      C.Text (Just reference) role _ sources | reference == startupReference -> C.Text (Just reference) role body sources
      other -> other
C.putContext (over C.contextBlocks (map (startupReplace "startup-refreshed")) startupCurrent)
display True
