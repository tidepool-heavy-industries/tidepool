let busyLabel = "busy" :: Label
busyWork <- unfold (batch campaign group) (child @Text (coding projectHead (assignment busyLabel ("fixture-busy" :: Text))))
