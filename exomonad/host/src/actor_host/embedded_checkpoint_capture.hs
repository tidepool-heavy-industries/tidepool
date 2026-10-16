checkpoint "embedded hosted checkpoint" >>= \result -> pure (either (const False) (const True) result)
