module CheckedValueQuoterProducer (__result) where

import Language.Haskell.TH.Quote (QuasiQuoter)
import qualified MetadataQuoter

__result :: QuasiQuoter
__result = MetadataQuoter.answer
