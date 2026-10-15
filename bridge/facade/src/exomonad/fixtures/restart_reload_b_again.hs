Right unchangedB <- reloadSource ["Project.ExtraCheck"]
case unchangedB of
  ReloadUnchanged _ -> display True
  _ -> error "reloading the same accepted source changed its publication"
