job <- Cmd.background [bash|printf cancellation-prefix|]
report <- await (Cmd.awaitFinished job)
fmap Cmd.reportSource report
sleep (seconds 30)
("unreachable suffix" :: Text)
