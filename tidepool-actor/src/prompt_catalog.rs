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

/// Fingerprint of the hosted Haskell tool description and instructions that
/// join every Shoal actor's effective provider prompt. The composition root
/// combines this with its role-specific developer prompt fingerprint so cache
/// observations never silently omit the tool surface.
pub fn hosted_prompt_fingerprint() -> String {
    let mut hasher = blake3::Hasher::new();
    for body in [
        PromptId::HaskellToolDescription.body(),
        PromptId::HaskellToolInstructions.body(),
    ] {
        hasher.update(&(body.len() as u64).to_le_bytes());
        hasher.update(body.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
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
        "cleanup" | "clean" => Ok(include_str!("../../prompts/shoal/docs/cleanup.md")),
        "refinement" | "refine" | "followup" => {
            Ok(include_str!("../../prompts/shoal/docs/refinement.md"))
        }
        "lineage" | "status" | "trace" => {
            Ok(include_str!("../../prompts/shoal/docs/lineage.md"))
        }
        "recovery" | "recover" => Ok(include_str!("../../prompts/shoal/docs/recovery.md")),
        "help" | "topics" => {
            Ok("Shoal topics: request, unfold, watch, deadline, refinement, lineage, cleanup, recovery. Use `:doc <topic>`.")
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
        assert!(workbench_doc("cleanup").unwrap().contains("executeCleanup"));
        assert!(workbench_doc("refinement")
            .unwrap()
            .contains("retained handles"));
        assert!(workbench_doc("lineage").unwrap().contains(":trace"));
        assert!(workbench_doc("recovery").unwrap().contains(":recovery"));
        assert_eq!(hosted_prompt_fingerprint().len(), 64);
        assert!(workbench_doc("missing").is_err());
    }
}
