{-# LANGUAGE DataKinds #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RoleAnnotations #-}

-- | Compiler-issued suspension evidence. The constructor is private and both
-- indices are nominal, so extracted evidence cannot be retagged for another
-- input vector or continuation result.
module Tidepool.Internal.RequestSite (RequestSite, requestSiteIdentity) where

import Data.Kind (Type)

type role RequestSite nominal nominal
newtype RequestSite (inputs :: [Type]) reply = RequestSite Int

-- | Correlation for private actor settlement steps. This does not issue new
-- reply evidence; only the compiler can construct the indexed carrier.
requestSiteIdentity :: RequestSite inputs reply -> Int
requestSiteIdentity (RequestSite identity) = identity
