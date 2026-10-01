let busyLabel = [label|busy|]
busyWork <- unfoldDeferred (batch campaign group) (child @Text (coding projectHead (assignment busyLabel ("fixture-busy" :: Text))))
