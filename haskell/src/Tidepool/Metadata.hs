-- | Constructor metadata serialized beside a Tidepool IR program.
module Tidepool.Metadata (DCMeta(..)) where

import Data.Text (Text)
import Data.Word (Word64)

data DCMeta = DCMeta
  { dcmId          :: !Word64
  , dcmName        :: !Text
  , dcmTag         :: !Int
  , dcmArity       :: !Int
  , dcmBangs       :: ![Text]
  , dcmQualName    :: !Text
  , dcmFieldLabels :: ![Text]
  , dcmTypeName    :: !Text
  , dcmFieldTypes  :: ![Text]
  }
