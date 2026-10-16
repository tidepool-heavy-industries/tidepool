Right idleAnswer <- request @Text idleChild ("explicit-idle-child-request" :: Text) defaultRequestOptions
Right idleResult <- await (result idleAnswer)
display idleResult
