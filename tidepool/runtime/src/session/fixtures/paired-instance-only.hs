{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE FlexibleInstances #-}
instance PublicClass Bool where
  type PublicFamily Bool = Int
  publicClass _ = 61
