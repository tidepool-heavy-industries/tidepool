let foregroundPrefix = "prefix-preserved" :: Text
attempt <- do { first <- Cmd.run [bash|printf first|]; second <- Cmd.run [bash|printf second|]; pure (first, second) }
suffix <- Cmd.run [bash|printf suffix|]
(Cmd.stdout (fst attempt), Cmd.stdout (snd attempt), Cmd.stdout suffix, "suffix-resumed" :: Text)
