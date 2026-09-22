emit x = print x
emit (T.replicate 6000 ("p" :: Text)) >> pure (T.replicate 6000 ("v" :: Text))
