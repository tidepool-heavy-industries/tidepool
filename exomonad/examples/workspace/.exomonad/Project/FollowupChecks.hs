{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.FollowupChecks (bounded) where

import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

-- The commands and every behavioral assertion execute in the checked actor.
-- Command outcomes, job identity and preparation evidence stay typed.
bounded :: Member RecipeCheck effects => Eff effects ()
bounded = do
  owner <- root
  void $ turn owner $ Text.unlines
    [ "import qualified Tidepool.Command as Cmd"
    , "import Project.ParallelInvestigate"
    , "import Project.TestEvidence"
    , "let probe name = CommandProbe name name \"/tmp\" (Cmd.MiB 64) (Cmd.argv [\"printf\",name])"
    , "let pick _ choices = pure (Right (case choices of { [] -> Nothing; x : _ -> Just x }))"
    , "original <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\",\"-c\",\"exit 7\"]))"
    , "result <- followFailureWith pick \"find relevant diagnostics\" original [probe \"first\", probe \"second\", probe \"third\"]"
    ]
  assertCell owner "two distinct supplied diagnostics retain the exact failed original" $ Text.unlines
    [ "case (originalObservation result, diagnosticObservations result, followupStop result) of"
    , "  (ProbeObserved _ originalJob originalReceipt _ _, [ProbeObserved \"first\" firstJob firstReceipt _ _, ProbeObserved \"second\" secondJob secondReceipt _ _], FollowupBudgetSpent) -> originalJob == original && Cmd.commandOutcome originalReceipt == Cmd.CommandExited 7 && firstJob /= secondJob && firstJob /= original && secondJob /= original && Cmd.commandOutcome firstReceipt == Cmd.CommandExited 0 && Cmd.commandOutcome secondReceipt == Cmd.CommandExited 0"
    , "  _ -> False"
    ]
  void $ turn owner $ Text.unlines
    [ "let spec = FocusedSpec \"investigate fixture\" \"fixture-source\" \"fixture-package\" \"lib\" \"fixture::one\" 1"
    , "focused <- collectFocused (PreparedFocusedRun spec original)"
    , "one <- followFailureWith pick \"known failed preparation\" (Cmd.job (focusedCommand focused)) [probe \"first\"]"
    ]
  assertCell owner "focused evidence composes with one direct diagnostic" $ Text.unlines
    [ "case (focusedPreparation focused, originalObservation one, diagnosticObservations one, followupStop one) of"
    , "  (PreparationUnknown, ProbeObserved _ job receipt _ _, [ProbeObserved \"first\" _ _ _ _], NoProbeNeeded) -> job == original && Cmd.commandOutcome receipt == Cmd.CommandExited 7 && not (focusedPassed focused)"
    , "  _ -> False"
    ]
  void $ turn owner "none <- followFailureWith (\\_ _ -> pure (Right Nothing)) \"uncertain\" original [probe \"unrun\"]"
  assertCell owner "abstention admits no diagnostic"
    "case followupStop none of { NoProbeNeeded -> null (diagnosticObservations none); _ -> False }"
  void $ turn owner "good <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"true\"]))\ngoodResult <- followFailureWith pick \"already successful\" good [probe \"unrun\"]"
  assertCell owner "successful original skips diagnostics"
    "case followupStop goodResult of { OriginalNotFailed -> null (diagnosticObservations goodResult); _ -> False }"
  void $ turn owner "refused <- followFailureWith pick \"invalid probes\" original [probe \"duplicate\", probe \"duplicate\"]"
  assertCell owner "duplicate probes preserve original failure without admission"
    "case (followupStop refused, originalObservation refused) of { (InvalidFollowupProbes (DuplicateAvailableProbe \"duplicate\"), ProbeObserved _ job receipt _ _) -> job == original && Cmd.commandOutcome receipt == Cmd.CommandExited 7 && null (diagnosticObservations refused); _ -> False }"
  void $ turn owner "cancelled <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sleep\",\"30\"]))\nCmd.cancel cancelled\ncancelledResult <- followFailureWith pick \"cancelled\" cancelled [probe \"unrun\"]"
  assertCell owner "cancelled original admits no diagnostic"
    "case followupStop cancelledResult of { OriginalNotDiagnosable receipt -> Cmd.commandOutcome receipt == Cmd.CommandCancelled && null (diagnosticObservations cancelledResult); _ -> False }"
  void $ turn owner $ Text.unlines
    [ "pending <- Cmd.start (Cmd.withMemory (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"sleep 0.05; exit 9\"]))"
    , "let delayed = CommandProbe \"waiting\" \"delayed diagnostic\" \"/tmp\" (Cmd.MiB 64) (Cmd.argv [\"sh\", \"-c\", \"sleep 0.05; printf done\"])"
    , "settled <- followFailureWith pick \"await original and diagnostic\" pending [delayed, probe \"second\", probe \"third\"]"
    ]
  assertCell owner "pending original and diagnostic settle in one continuation"
    "case (originalObservation settled, diagnosticObservations settled, followupStop settled) of { (ProbeObserved _ job receipt _ _, [ProbeObserved \"waiting\" _ waitingReceipt (Right out) _, ProbeObserved \"second\" _ _ _ _], FollowupBudgetSpent) -> job == pending && Cmd.commandOutcome receipt == Cmd.CommandExited 9 && Cmd.commandOutcome waitingReceipt == Cmd.CommandExited 0 && Cmd.pageText out == \"done\"; _ -> False }"
