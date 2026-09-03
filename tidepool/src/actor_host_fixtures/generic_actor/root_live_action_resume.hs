:{
do
  if sessionInput == (42 :: Int)
    then complete (pure ())
    else error "composed heterogeneous actor exits returned the wrong result"
:}
