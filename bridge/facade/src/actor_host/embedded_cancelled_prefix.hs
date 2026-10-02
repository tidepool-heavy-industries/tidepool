job <- Cmd.background [bash|printf cancellation-prefix|]
report <- waitFor (Cmd.awaitFinished job)
fmap Cmd.reportSource report
sleep (seconds 30)
("unreachable suffix" :: Text)
