{-# LANGUAGE QuasiQuotes #-}
import qualified Tidepool.Actor.Record as R
let leftTask = task [label|left|] "Investigate the left boundary" ["src/left.rs"] "Return findings with references; no code candidate required" sourceHead
let rightTask = task [label|right|] "Investigate the right boundary" ["src/right.rs"] "Return findings with references; no code candidate required" sourceHead
-- Descendants inherit the explicit retained prefix; the root uses selected task guidance.
Right localCheckpoint <- Tidepool.Actors.Exomonad.checkpoint "recursive-batch"
let localContext :: Branch CodingEffects Task (Outcome Text) -> Branch CodingEffects Task (Outcome Text)
    localContext branch = withLifetime ActorOwned (if groupName == "first" then branch else withContext (fromCheckpoint localCheckpoint) branch)
let sink :: WorkSink (Outcome Text)
    sink = notifyWork me (workMessage (\result -> T.pack (show (result :: Outcome Text))))
let group = if groupName == "first" then batch "recursive-check" groupName else subgroup groupName
(work, answerer) <- if groupName == "subcomponents" then do
  Right (admitted, answers) <- unfoldAnsweredWork me group
    [ ("left", leftTask, localContext . lunaTask [label|left|] Medium)
    , ("right", rightTask, localContext . lunaTask [label|right|] Medium)
    ] sink
  pure (admitted, Just answers)
  else do
    Right admitted <- unfoldWork group
      [ workChild "left" (localContext (lunaTask [label|left|] Medium leftTask))
      , workChild "right" (localContext (lunaTask [label|right|] Medium rightTask))
      ] sink
    pure (admitted, Nothing)
let [(_, left, leftProgress), (_, right, rightProgress)] = batchMembers work
releaseCheckpoint localCheckpoint
