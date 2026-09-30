{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE FlexibleInstances #-}

instance PublicClass Bool where
  type PublicFamily Bool = Int
  publicClass _ = 22

privateWinner :: Bool
privateWinner = True
