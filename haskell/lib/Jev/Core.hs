-- | The polymorphic core: everything in "Jev.Operators" without the JSON
-- type fixed. Import this to build a facade over another JSON value type.
module Jev.Core
  ( module Jev.Core.Json
  , module Jev.Core.Contract
  , module Jev.Core.Schema
  ) where

import Jev.Core.Contract
import Jev.Core.Json
import Jev.Core.Schema
