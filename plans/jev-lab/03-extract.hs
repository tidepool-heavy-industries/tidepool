afterArrow :: Text -> Maybe Text
afterArrow l = T.strip <$> (T.stripPrefix "-->" . T.dropWhile (/= '-') =<< (if "-->" `T.isInfixOf` l then Just l else Nothing))

labelOf :: Text -> Maybe Text
labelOf l | "note:" `T.isPrefixOf` T.strip l = Just (T.strip l)
          | "help:" `T.isPrefixOf` T.strip l = Just (T.strip l)
          | otherwise = Nothing

sitesOf :: [Text] -> [(Text, Text)]
sitesOf = go "primary"
  where go _ [] = []
        go lab (l:ls) = case afterArrow l of
          Just loc -> (lab, loc) : go lab ls
          Nothing -> go (maybe lab id (labelOf l)) ls

codeOf :: Text -> Text
codeOf h = maybe "" (T.takeWhile (/= ']')) (T.stripPrefix "[" (T.dropWhile (/= '[') h))

object [ "a" .= [ object ["code" .= codeOf (head c), "sites" .= sitesOf c] | c <- diagChunks rawA ]
       , "b" .= [ object ["code" .= codeOf (head c), "sites" .= sitesOf c] | c <- diagChunks rawB ] ]
