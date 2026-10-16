import Control.Lens (view)
import qualified Tidepool.Agent.Context as C

startupPublication <- C.getInitialContext
(startupIdentity, startupAssociation, startupOriginalText, startupReference) <- case startupPublication of
  C.InitialContextPublished identity association text reference -> pure (identity, association, text, reference)
  other -> error (T.pack (show other))

startupCurrent <- C.getContext
let startupMatches = [body | C.Text (Just reference) _ body _ <- view C.contextBlocks startupCurrent, reference == startupReference]
_ <- if startupMatches == [startupOriginalText] then pure () else error "startup publication target differs from retained result"
let startupTarget context = case filter ((== startupReference) . C.targetReference) (C.textTargets context) of
      [target] -> target
      _ -> error "startup target is missing or ambiguous"
C.ContextStaged startupRefreshTransaction <- C.stageContext
  (C.contextPatch [C.replaceText (startupTarget startupCurrent) "startup-refreshed"])
display True
