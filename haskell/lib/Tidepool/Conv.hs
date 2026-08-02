{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Speculative dialogue trees — the free-monad @Conv@ modality.
--
-- The model authors, in ONE turn, a subtree of the LIKELY next few exchanges;
-- 'runConv' walks it through the harness's dialog machinery, one operator
-- interaction per node, with NO model round-trip between nodes. Control returns
-- to the model only where the tree says 'reask' (a branch the model did not
-- anticipate). One authored turn thus amortizes several interactions — a clean
-- question/answer tempo instead of an essay per turn.
--
-- @Conv@ is a free monad, so you write ordinary do-notation for the linear
-- tempo and 'menu' for the splits (branches are DATA — sub-@Conv@s — not a
-- @case@):
--
-- > booking = do
-- >   narrate "Let's get you booked."
-- >   n <- pick "How many nights?" ["1","2","3+"]
-- >   menu "Breakfast?"
-- >     [ ("Yes", do diet <- multi "Dietary needs" ["veg","halal","none"]
-- >                  done ("Booked " <> n <> " nights, breakfast: " <> T.intercalate "," diet))
-- >     , ("No",  done ("Booked " <> n <> " nights, no breakfast")) ]
-- >
-- > result <- runConv booking      -- :: M Value  ({status, summary})
--
-- Because it is a free monad the SAME value could later be walked by a native
-- (Rust) interpreter for whole-tree prefetch/rendering; 'runConv' is the
-- effect interpreter that reuses everything already wired (dialogAsk suspend/
-- resume, hole rendering, the observatory).
module Tidepool.Conv
  ( Conv
  , narrate
  , menu
  , input
  , pick
  , multi
  , reask
  , done
  , runConv
  ) where

import Prelude
import Data.Text (Text)

import Tidepool.Effects (M, dialogAsk)
import Tidepool.Aeson.Value (Value, object, (.=))
import qualified Tidepool.Ui as U
import qualified Tidepool.Form as F

-- | A speculative dialogue subtree yielding an @a@. A free monad over the
-- dialogue node functor: 'say'/'ask'/'pick'/'multi' are the linear steps,
-- 'menu' branches (each branch a sub-@Conv@), 'reask'/'done' are the terminals.
data Conv a
  = Pure a
  | -- | narration folded into the NEXT node's card (no extra click)
    Say Text (Conv a)
  | -- | branch on the picked label; a label NOT listed => 'reask' (default)
    Menu Text [(Text, Conv a)]
  | -- | free-text answer, continue with it
    Ask Text (Text -> Conv a)
  | -- | pick one of the options (returns the key), continue with it
    Pick Text [Text] (Text -> Conv a)
  | -- | pick a SUBSET (checkboxes), continue with it
    Multi Text [Text] ([Text] -> Conv a)
  | -- | hand control back to the model, with the answers so far
    Reask
  | -- | end the dialogue with a summary
    Done Text

instance Functor Conv where
  fmap f c = c >>= (Pure . f)

instance Applicative Conv where
  pure = Pure
  cf <*> cx = cf >>= \f -> fmap f cx

instance Monad Conv where
  Pure a >>= k = k a
  Say t c >>= k = Say t (c >>= k)
  Menu p bs >>= k = Menu p [(l, c >>= k) | (l, c) <- bs]
  Ask p f >>= k = Ask p (\t -> f t >>= k)
  Pick p os f >>= k = Pick p os (\t -> f t >>= k)
  Multi p os f >>= k = Multi p os (\xs -> f xs >>= k)
  Reask >>= _ = Reask
  Done t >>= _ = Done t

-- | A narrated beat. Its prose is folded into the NEXT input node's card, so it
-- costs no extra operator click (a trailing 'say' with no following input is
-- shown as a final card).
narrate :: Text -> Conv ()
narrate t = Say t (Pure ())

-- | A branching choice: present @prompt@ + the labels, and continue into the
-- picked branch. An answer NOT among the labels returns control to the model.
menu :: Text -> [(Text, Conv a)] -> Conv a
menu = Menu

-- | A free-text question; continue with the operator's text.
input :: Text -> Conv Text
input p = Ask p Pure

-- | A single choice among @options@ that does NOT branch — continue with the
-- selected key (use it in the summary / a later condition).
pick :: Text -> [Text] -> Conv Text
pick p os = Pick p os Pure

-- | A checkbox subset of @options@; continue with the selected keys.
multi :: Text -> [Text] -> Conv [Text]
multi p os = Multi p os Pure

-- | Return control to the model (an unanticipated branch); the answers so far
-- are in the transcript for the model's next turn.
reask :: Conv a
reask = Reask

-- | End the dialogue with a summary line.
done :: Text -> Conv a
done = Done

-- | Interpret a 'Conv' through the harness dialog machinery: one operator
-- interaction per node (each a @dialogForm@ suspension), following branches by
-- the answer, until 'done' or 'reask'. Returns @{status, summary}@ — @status@ is
-- @"done"@ (with the summary) or @"reask"@ (the model continues next turn).
--
-- Pending 'say' narration is threaded into the next node's card via
-- 'Tidepool.Form.prose' so it shares the same click.
runConv :: Conv a -> M Value
runConv = go []
  where
    go :: [Text] -> Conv a -> M Value
    go ps c = case c of
      Pure _ -> flush ps (statusObj "done" "")
      Say t k -> go (ps ++ [t]) k
      Done t -> flush ps (statusObj "done" t)
      Reask -> flush ps (statusObj "reask" "")
      Menu p bs -> do
        r <- F.dialogForm (withProse ps (F.choiceField p [(l, l) | (l, _) <- bs]))
        case r of
          Right l -> maybe (go [] Reask) (go []) (lookup l bs)
          Left _ -> go [] Reask
      Ask p f -> do
        r <- F.dialogForm (withProse ps (F.textField p))
        continue f r
      Pick p os f -> do
        r <- F.dialogForm (withProse ps (F.choiceField p [(o, o) | o <- os]))
        continue f r
      Multi p os f -> do
        r <- F.dialogForm (withProse ps (F.multiChoiceField p [(o, o) | o <- os]))
        continue f r

    -- Feed a decoded answer into the continuation; a decode failure returns
    -- control to the model rather than aborting.
    continue :: (b -> Conv a) -> Either F.FormError b -> M Value
    continue f (Right x) = go [] (f x)
    continue _ (Left _) = go [] Reask

    -- Prepend pending narration to a form as non-consuming display prose.
    withProse :: [Text] -> F.Form b -> F.Form b
    withProse ps form = foldr (\t acc -> F.prose t *> acc) form ps

    -- A terminal reached with unshown narration: render it as a final card so
    -- it is not lost, then yield the outcome.
    flush :: [Text] -> Value -> M Value
    flush [] out = pure out
    flush ps out = do
      _ <- dialogAsk (U.card "" (map U.prose ps))
      pure out

statusObj :: Text -> Text -> Value
statusObj st summary = object ["status" .= st, "summary" .= summary]
