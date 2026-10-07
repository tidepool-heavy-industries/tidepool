{-# LANGUAGE OverloadedStrings #-}
-- | Versioned rich presentation encoding, shared by display and form answers.
module Tidepool.View.Wire (encodeView) where
import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Aeson.Value
import Tidepool.Inspection.Tree
import Tidepool.View.Types

-- | One shared budget bounds demanded text and layout nodes. Media bytes are
-- admitted separately by the host's format and byte limits. A clipped nested
-- inspection explicitly reports unavailable detail because it has no callback.
encodeView :: Int -> View -> Value
encodeView budget = fst . go (max 0 budget)
  where
    exhausted = object ["kind" .= ("inspection" :: Text), "text" .= ("" :: Text), "has_more" .= True, "unavailable" .= True]
    go n _ | n <= 0 = (exhausted, 0)
    go n (PlainText t) = textual n "text" t
    go n (Markdown t) = textual n "markdown" t
    go n (Row xs) = layout n "row" xs
    go n (Column xs) = layout n "column" xs
    go n (Caption v t) =
      let (body,remaining) = go (n-1) v
          (label,clipped) = rawText remaining t
      in (object ["kind" .= ("caption" :: Text), "body" .= body, "text" .= label, "truncated" .= clipped], max 0 (remaining-T.length label))
    go n (Image (ImageData format bytes) alt) =
      let (label,clipped) = rawText (n-1) alt
      in (object ["kind" .= ("image" :: Text), "source" .= object ["kind" .= ("data" :: Text), "mime" .= mime format, "base64" .= bytes], "alt" .= label, "truncated" .= clipped], max 0 (n-1-T.length label))
    go n (Vector (SvgDocument source)) = (object ["kind" .= ("svg" :: Text), "source" .= source],n-1)
    go n (Inspection tree) =
      let (t,rest,unavailable) = renderTree (n-1) tree
          more = maybe False (const True) rest
      in (object ["kind" .= ("inspection" :: Text), "text" .= t, "has_more" .= more, "unavailable" .= (unavailable || more)],max 0 (n-1-T.length t))
    textual n kind t =
      let (rendered,clipped) = rawText (n-1) t
      in (object ["kind" .= (kind :: Text), "text" .= rendered, "truncated" .= clipped],max 0 (n-1-T.length rendered))
    layout n kind xs = let (children,remaining) = childrenWith (n-1) xs
      in (object ["kind" .= (kind :: Text), "children" .= children],remaining)
    childrenWith n _ | n <= 0 = ([exhausted],0)
    childrenWith n [] = ([],n)
    childrenWith n (v:vs) = let (child,remaining) = go n v; (children,remaining') = childrenWith remaining vs
      in (child:children,remaining')
    mime PNG = "image/png" :: Text
    mime JPEG = "image/jpeg"
    mime WebP = "image/webp"
