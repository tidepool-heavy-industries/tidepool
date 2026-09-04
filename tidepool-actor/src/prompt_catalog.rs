#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptId {
    HaskellToolDescription,
    HaskellToolInstructions,
}

impl PromptId {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 2] = [Self::HaskellToolDescription, Self::HaskellToolInstructions];

    pub(crate) fn artifact(self) -> PromptArtifact {
        match self {
            Self::HaskellToolDescription => PromptArtifact {
                id: self,
                role: PromptRole::HostedToolDescription,
                body: include_str!("../../prompts/shoal/haskell-tool-description.md"),
            },
            Self::HaskellToolInstructions => PromptArtifact {
                id: self,
                role: PromptRole::HostedToolInstructions,
                body: include_str!("../../prompts/shoal/haskell-tool-instructions.md"),
            },
        }
    }

    pub(crate) fn body(self) -> &'static str {
        self.artifact().body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptRole {
    HostedToolDescription,
    HostedToolInstructions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptArtifact {
    pub(crate) id: PromptId,
    pub(crate) role: PromptRole,
    pub(crate) body: &'static str,
}

pub(crate) fn workbench_doc(topic: &str) -> Result<&'static str, String> {
    match topic {
        "request" | "requests" => Ok(include_str!("../../prompts/shoal/docs/request.md")),
        "unfold" | "fork" | "forks" => Ok(include_str!("../../prompts/shoal/docs/unfold.md")),
        "watch" | "watches" | "poll" => Ok(include_str!("../../prompts/shoal/docs/watch.md")),
        "deadline" | "deadlines" | "duration" => {
            Ok(include_str!("../../prompts/shoal/docs/deadline.md"))
        }
        "help" | "topics" => {
            Ok("Shoal topics: request, unfold, watch, deadline. Use `:doc <topic>`.")
        }
        other => Err(format!(
            "unknown Shoal documentation topic `{other}`; use `:doc topics`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_nonempty_and_role_typed() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert_eq!(
            artifacts.map(|artifact| artifact.role),
            [
                PromptRole::HostedToolDescription,
                PromptRole::HostedToolInstructions,
            ]
        );
        let description = PromptId::HaskellToolDescription.body();
        assert!(description.contains("typed `sessionReply`"));
        assert!(description.contains("`:status`"));
        assert!(description.contains("Ordinary model-response termination"));
        assert!(!description.contains("assemble"));
        assert!(workbench_doc("unfold").unwrap().contains("Forked a"));
        assert!(workbench_doc("missing").is_err());
    }
}
