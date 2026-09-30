module PrefixCheck where
import Prelude
import Foreign (hidden)
import Tidepool.Session.Lib.G7
import Tidepool.Session.Val.G9 (id)
import qualified Tidepool.Session.Val.G8
__result :: IO (Int, Int, Int, Int)
__result = pure (id, Tidepool.Session.Lib.G7.id 2, Foreign.hidden, Tidepool.Session.Val.G8.older)
