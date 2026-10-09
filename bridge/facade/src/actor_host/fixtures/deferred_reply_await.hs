completed <- await (observed deferredWatch)
display (case completed of { Right value -> value == ("first accepted failure" :: Text); _ -> False })
