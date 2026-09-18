either (\e -> object ["jev_error" .= T.pack (show e)]) (\r -> toJSON (J.answers r)) ansA
