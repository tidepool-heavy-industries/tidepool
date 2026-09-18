secondaries :: [Text] -> [Text]
secondaries c = [ lab <> " @ " <> loc | (lab, loc) <- sitesOf c, lab /= "primary" ]

primaries :: [Text] -> [Text]
primaries c = [ loc | (lab, loc) <- sitesOf c, lab == "primary" ]

groupKey :: [Text] -> (Text, [Text])
groupKey c = (head c, L.sort (secondaries c))

mechanicalGroups :: Text -> [((Text, [Text]), [[Text]])]
mechanicalGroups raw = Map.toList (Map.fromListWith (flip (++)) [ (groupKey c, [c]) | c <- diagChunks raw ]) 

object [ "a_groups" .= [ object ["headline" .= fst k, "shared" .= snd k, "members" .= length ms, "sites" .= concatMap primaries ms] | (k, ms) <- mechanicalGroups rawA ]
       , "b_groups" .= [ object ["headline" .= fst k, "shared" .= snd k, "members" .= length ms, "sites" .= concatMap primaries ms] | (k, ms) <- mechanicalGroups rawB ] ]
