//! Provider-neutral parsers for model-authored response text.
//!
//! Tidepool's primary resident-Haskell protocol uses ordinary assistant text:
//! explicitly tagged Haskell fences are runnable, while prose and other fence
//! languages remain inert. This crate identifies those regions but never
//! compiles or executes them.

/// Extract every explicitly tagged Haskell block from an assistant response.
///
/// Tags are case-insensitive and may carry trailing words. An opener may occur
/// after prose on its line, fused close/reopen fences are supported, and an
/// unterminated final block runs. Empty blocks are omitted and trailing
/// whitespace is removed so generated turn templates receive stable source.
pub fn extract_haskell_blocks(response: &str) -> Vec<String> {
    enum Fence {
        Haskell(String),
        Other,
    }

    fn open_fence(source: &str) -> Option<Fence> {
        let language = source.strip_prefix("```")?;
        let tag = language.split_whitespace().next().unwrap_or("");
        Some(
            if tag.eq_ignore_ascii_case("haskell") || tag.eq_ignore_ascii_case("hs") {
                Fence::Haskell(String::new())
            } else {
                Fence::Other
            },
        )
    }

    fn close(blocks: &mut Vec<String>, fence: Option<Fence>) {
        if let Some(Fence::Haskell(body)) = fence {
            let body = body.trim_end().to_string();
            if !body.is_empty() {
                blocks.push(body);
            }
        }
    }

    let mut blocks = Vec::new();
    let mut fence: Option<Fence> = None;
    for line in response.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence.as_mut() {
            if let Some(rest) = trimmed.strip_prefix("```") {
                // A language tag inside an inert example is still part of
                // that example; only a bare fence (or fused close/open)
                // ends it.
                let closes = matches!(open, Fence::Haskell(_))
                    || rest.trim().is_empty()
                    || rest.starts_with("```");
                if closes {
                    close(&mut blocks, fence.take());
                    fence = open_fence(rest.trim_start());
                }
            } else if let Fence::Haskell(body) = open {
                body.push_str(line);
                body.push('\n');
            }
        } else if let Some(index) = trimmed.find("```") {
            fence = open_fence(&trimmed[index..]);
        }
    }
    close(&mut blocks, fence);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_all_haskell_blocks_in_order() {
        let response = "First:\n```haskell\ndata Mood = Rested | Wired\n```\n\
                        Then: ```hs\nmood <- askUser @Mood \"how?\"\n```";
        assert_eq!(
            extract_haskell_blocks(response),
            [
                "data Mood = Rested | Wired",
                "mood <- askUser @Mood \"how?\""
            ]
        );
    }

    #[test]
    fn ignores_prose_bare_fences_and_other_languages() {
        let response = "prose\n```\nplain\n```\n```text\nquoted\n```";
        assert!(extract_haskell_blocks(response).is_empty());
    }

    #[test]
    fn supports_fused_close_and_reopen() {
        let response = "```haskell\ndata X = X\n``````haskell\ndata Y = Y\n```";
        assert_eq!(
            extract_haskell_blocks(response),
            ["data X = X", "data Y = Y"]
        );
    }

    #[test]
    fn a_haskell_example_inside_another_fence_is_not_runnable() {
        let response = "```text\nexample:\n```haskell\npure (error \"not runnable\")\n```\n\
                        ```haskell\npure ()\n```";
        assert_eq!(extract_haskell_blocks(response), ["pure ()"]);
    }

    #[test]
    fn a_fused_close_can_open_haskell_after_an_inert_fence() {
        let response = "```text\nexample\n``````haskell\npure ()\n```";
        assert_eq!(extract_haskell_blocks(response), ["pure ()"]);
    }
}
