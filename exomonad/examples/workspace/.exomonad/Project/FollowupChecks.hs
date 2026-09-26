{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module Project.FollowupChecks (bounded) where
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Check
bounded :: Member RecipeCheck effects => Eff effects ()
bounded = do
  owner <- root
  void $ turn owner $ T.unlines
    [ "import qualified Tidepool.Command as Cmd"
    , "import Project.ParallelInvestigate"
    , "let probe name = CommandProbe name name \"/tmp\" (Cmd.MiB 64) (Cmd.argv [\"printf\",name])"
    , "let pick _ choices = pure (Right (case choices of { [] -> Nothing; x : _ -> Just x }))"
    , "original <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\",\"-c\",\"exit 7\"]))"
    , "Cmd.await original"
    , "result <- followFailureWith pick \"find relevant diagnostics\" original [probe \"first\", probe \"second\", probe \"third\"]"
    ]
  result <- turn owner "inspectFull result"
  check "two supplied diagnostics run and retain original failure"
    (all (`T.isInfixOf` output result) ["CommandExited 7", "first", "second", "FollowupBudgetSpent"]
      && not ("third" `T.isInfixOf` output result))
  none <- turn owner "none <- followFailureWith (\\_ _ -> pure (Right Nothing)) \"uncertain\" original [probe \"unrun\"]\ninspectFull none"
  check "abstention runs no diagnostic" ("NoProbeNeeded" `T.isInfixOf` output none && not ("unrun" `T.isInfixOf` output none))
  void $ turn owner "good <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"true\"]))\nCmd.await good"
  good <- turn owner "goodResult <- followFailureWith pick \"already successful\" good [probe \"unrun\"]\ninspectFull goodResult"
  check "success skips all judgment and diagnostic work" ("OriginalNotFailed" `T.isInfixOf` output good && not ("unrun" `T.isInfixOf` output good))
  refused <- turn owner "refused <- followFailureWith pick \"invalid probe\" original [probe \"duplicate\", probe \"duplicate\"]\ninspectFull refused"
  check "invalid diagnostics preserve original failure" ("InvalidFollowupProbes" `T.isInfixOf` output refused && "CommandExited 7" `T.isInfixOf` output refused)
  void $ turn owner "cancelled <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\",\"30\"]))\nCmd.cancel cancelled\nCmd.await cancelled"
  cancelled <- turn owner "cancelledResult <- followFailureWith pick \"cancelled\" cancelled [probe \"unrun\"]\ninspectFull cancelledResult"
  check "cancellation never starts diagnostics" ("OriginalNotDiagnosable" `T.isInfixOf` output cancelled && not ("unrun" `T.isInfixOf` output cancelled))
