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
    fn opens_haskell(source: &str) -> bool {
        let Some(language) = source.strip_prefix("```") else {
            return false;
        };
        let tag = language.split_whitespace().next().unwrap_or("");
        matches!(tag.to_ascii_lowercase().as_str(), "haskell" | "hs")
    }

    fn close(blocks: &mut Vec<String>, body: &mut Option<String>) {
        if let Some(body) = body.take() {
            let body = body.trim_end().to_string();
            if !body.is_empty() {
                blocks.push(body);
            }
        }
    }

    let mut blocks = Vec::new();
    let mut body: Option<String> = None;
    for line in response.lines() {
        let trimmed = line.trim_start();
        if body.is_some() {
            if let Some(rest) = trimmed.strip_prefix("```") {
                close(&mut blocks, &mut body);
                if opens_haskell(rest.trim_start()) {
                    body = Some(String::new());
                }
            } else if let Some(body) = body.as_mut() {
                body.push_str(line);
                body.push('\n');
            }
        } else if let Some(index) = trimmed.find("```") {
            if opens_haskell(&trimmed[index..]) {
                body = Some(String::new());
            }
        }
    }
    close(&mut blocks, &mut body);
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
}
