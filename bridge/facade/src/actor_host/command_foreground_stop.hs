let foregroundPrefix = "prefix-preserved" :: Text
attempt <- do { first <- Cmd.run [bash|printf done|]; Cmd.run [bash|printf forbidden-inner|] }
Cmd.run [bash|printf forbidden-suffix|]
