n <- J.ask (J.state (#task := ("t" :: Text))) (#q := J.noul "Is the sky blue?")
fmap (\a -> a.q.yes) n
