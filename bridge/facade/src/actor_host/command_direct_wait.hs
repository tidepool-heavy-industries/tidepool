import qualified Tidepool.Agent.Watch as W
job <- Cmd.start [bash|printf direct-wait|]
report <- W.await (Cmd.awaitFinished job)
case report of { Right finished -> (Cmd.commandOutcome (Cmd.reportResult finished), fmap Cmd.sourceCommit (Cmd.reportSource finished), Cmd.reportOutputComplete finished); Left issue -> error ("direct wait was unavailable: " <> T.pack (show issue)) }
"direct-wait-captured" :: Text
