use std::borrow::Cow;

const HOSTED_DESCRIPTION_LIMIT: usize = 1024;
const HASKELL_TOOL_DESCRIPTION: &str = include_str!("../../prompts/haskell-tool-description.md");
const HASKELL_TOOL_INSTRUCTIONS: &str = include_str!("../../prompts/haskell-tool-instructions.md");

const fn utf8_char_count(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut count = 0;
    while index < bytes.len() {
        if bytes[index] & 0b1100_0000 != 0b1000_0000 {
            count += 1;
        }
        index += 1;
    }
    count
}

const _: () = assert!(
    utf8_char_count(HASKELL_TOOL_DESCRIPTION) <= HOSTED_DESCRIPTION_LIMIT,
    "hosted Haskell tool description exceeds the provider limit"
);
const _: () = assert!(
    utf8_char_count(HASKELL_TOOL_INSTRUCTIONS) <= HOSTED_DESCRIPTION_LIMIT,
    "hosted Haskell tool instructions exceed the provider limit"
);

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
                body: HASKELL_TOOL_DESCRIPTION,
            },
            Self::HaskellToolInstructions => PromptArtifact {
                id: self,
                role: PromptRole::HostedToolInstructions,
                body: HASKELL_TOOL_INSTRUCTIONS,
            },
        }
    }

    pub(crate) fn body(self) -> &'static str {
        self.artifact().body
    }
}

/// Fingerprint of the hosted Haskell tool description and instructions that
/// join every Exomonad actor's effective provider prompt. The composition root
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

/// `workspace_modules` names the Exomonad workspace's own configured Haskell
/// modules (`FrozenWorkspace::modules`, threaded through
/// `ActorWorkbenchSource::with_workspace_modules`). Empty for the operator
/// workbench, tests, and a workspace with no `[haskell] modules`; in that
/// case the topics list and unknown-topic error are unchanged from before
/// workspace modules existed.
/// The skills an Exomonad workspace ships, named so an unknown topic can point at
/// the one that answers it. These are the same names the `topics` body lists.
const SHIPPED_SKILLS: [&str; 11] = [
    "exomonad-jev",
    "exomonad-orchestrate",
    "exomonad-unfold",
    "exomonad-workbench",
    "exomonad-cleanup",
    "exomonad-fork",
    "exomonad-coordinate",
    "exomonad-review",
    "exomonad-command",
    "exomonad-define-actors",
    "exomonad-agent-spec",
];

/// A topic naming a shipped skill, either bare (`command`) or in full
/// (`exomonad-command`).
fn skill_for_topic(topic: &str) -> Option<&'static str> {
    SHIPPED_SKILLS
        .into_iter()
        .find(|skill| *skill == topic || skill.strip_prefix("exomonad-") == Some(topic))
}

pub(crate) fn workbench_doc(
    topic: &str,
    workspace_modules: &[String],
) -> Result<Cow<'static, str>, String> {
    match topic {
        "tree" | "worktree" | "worktrees" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/tree.md")))
        }
        "workbench" => Ok(Cow::Borrowed(include_str!(
            "../../prompts/docs/workbench.md"
        ))),
        "request" | "requests" => Ok(Cow::Borrowed(include_str!("../../prompts/docs/request.md"))),
        "unfold" | "fork" | "forks" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/unfold.md")))
        }
        "jev" => Ok(Cow::Borrowed(include_str!("../../prompts/docs/jev.md"))),
        "actors" | "actor" | "record" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/actors.md")))
        }
        "watch" | "watches" | "poll" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/watch.md")))
        }
        "deadline" | "deadlines" | "duration" => Ok(Cow::Borrowed(include_str!(
            "../../prompts/docs/deadline.md"
        ))),
        "cleanup" | "clean" => Ok(Cow::Borrowed(include_str!("../../prompts/docs/cleanup.md"))),
        "refinement" | "refine" | "followup" => Ok(Cow::Borrowed(include_str!(
            "../../prompts/docs/refinement.md"
        ))),
        "lineage" | "status" | "trace" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/lineage.md")))
        }
        "recovery" | "recover" => Ok(Cow::Borrowed(include_str!(
            "../../prompts/docs/recovery.md"
        ))),
        "reflect" | "conversation" | "history" => {
            Ok(Cow::Borrowed(include_str!("../../prompts/docs/reflect.md")))
        }
        "help" | "topics" => {
            let mut body = String::from(
                "Exomonad topics: tree (worktree), workbench, request, unfold, watch, deadline, refinement, lineage, cleanup, recovery, jev, actors, reflect. Use hosted `lookup` with `doc <topic>`.\n\
                 Load the skill first where one exists; a topic is the fallback. Workspace skills: exomonad-jev (judgment-model packets and gates), exomonad-orchestrate (the implement/review/repair/merge loop as one record actor), exomonad-unfold (multi-child unfolds and reading a child's commit), exomonad-workbench (cells that typecheck the first time), exomonad-cleanup (retiring workers and groups), exomonad-agent-spec (your own tools, after-tool slot, and watchdog heuristics on children — all read, edited and reloaded live), exomonad-fork, exomonad-coordinate, exomonad-review, exomonad-command, exomonad-define-actors.",
            );
            if !workspace_modules.is_empty() {
                body.push_str(
                    "\nWorkspace modules (compiled into this session; lookup a name to browse it): ",
                );
                body.push_str(&workspace_modules.join(", "));
            }
            Ok(Cow::Owned(body))
        }
        other => {
            let mut message = format!("unknown Exomonad documentation topic `{other}`");
            // A topic that names a shipped skill is the most likely thing the
            // asker actually wanted, and the seat's own rule is to load the
            // skill before falling back to a topic. Saying so costs one line
            // and saves a search; a live lead asked `doc command` while
            // `exomonad-command` sat unmentioned, then spent four lookup rounds
            // guessing names.
            if let Some(skill) = skill_for_topic(other) {
                message.push_str(&format!(
                    "; the `{skill}` skill covers this — load it first, a topic is the fallback"
                ));
            }
            message.push_str(
                "; topics: tree (worktree), workbench, request, unfold, watch, deadline, refinement, lineage, cleanup, recovery, jev, actors; `doc topics` lists the workspace skills"
            );
            if !workspace_modules.is_empty() {
                message.push_str(&format!(
                    "; workspace modules: {}",
                    workspace_modules.join(", ")
                ));
            }
            Err(message)
        }
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
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.body.chars().count() <= HOSTED_DESCRIPTION_LIMIT));
        assert!(workbench_doc("unfold", &[]).unwrap().contains("Response a"));
        assert!(workbench_doc("cleanup", &[])
            .unwrap()
            .contains("executeCleanup"));
        assert!(workbench_doc("refinement", &[])
            .unwrap()
            .contains("retained handles"));
        assert!(workbench_doc("lineage", &[]).unwrap().contains("trace"));
        assert!(workbench_doc("recovery", &[]).unwrap().contains("recovery"));
        assert!(workbench_doc("actors", &[])
            .unwrap()
            .contains("R.settlement"));
        // Every topic with a workspace skill names it on its last line, and the
        // topic listing names the skills beside the topics. `jev` instead
        // links the skill inline and says so, rather than duplicating its
        // content behind a trailer.
        for (topic, skill) in [
            ("actors", "exomonad-define-actors"),
            ("cleanup", "exomonad-cleanup"),
            ("unfold", "exomonad-unfold"),
            ("workbench", "exomonad-workbench"),
        ] {
            let body = workbench_doc(topic, &[]).unwrap();
            assert_eq!(
                body.trim_end().lines().last(),
                Some(format!("skill: {skill}").as_str()),
                "`doc {topic}` must end by naming {skill}"
            );
            assert!(workbench_doc("topics", &[]).unwrap().contains(skill));
        }
        assert!(workbench_doc("jev", &[]).unwrap().contains("exomonad-jev"));
        assert!(workbench_doc("topics", &[]).unwrap().contains("exomonad-jev"));
        assert_eq!(hosted_prompt_fingerprint().len(), 64);
        assert!(workbench_doc("missing", &[]).is_err());
    }

    #[test]
    fn an_unknown_topic_naming_a_shipped_skill_says_so() {
        // A live lead asked `doc command` while `exomonad-command` — whose whole
        // subject is running commands — went unmentioned, then spent four
        // lookup rounds guessing constructor names.
        let refusal = workbench_doc("command", &[]).unwrap_err();
        assert!(
            refusal.contains("`exomonad-command` skill covers this"),
            "{refusal}"
        );
        assert!(refusal.starts_with("unknown Exomonad documentation topic `command`"));

        // The full name works too, and so does every other shipped skill that
        // is not already a topic in its own right.
        for topic in ["exomonad-command", "review", "coordinate", "orchestrate"] {
            let refusal = workbench_doc(topic, &[]).unwrap_err();
            assert!(
                refusal.contains("skill covers this"),
                "`doc {topic}`: {refusal}"
            );
        }

        // A topic that names nothing still refuses plainly, with no skill line.
        let missing = workbench_doc("missing", &[]).unwrap_err();
        assert!(!missing.contains("skill covers this"), "{missing}");
    }

    #[test]
    fn topics_and_unknown_topic_error_name_configured_workspace_modules() {
        let modules = vec![
            "Project.Investigate".to_string(),
            "Project.Recall".to_string(),
        ];

        // No workspace modules: identical to a workspace with none configured.
        assert!(!workbench_doc("topics", &[])
            .unwrap()
            .contains("Project.Investigate"));

        let topics = workbench_doc("topics", &modules).unwrap();
        assert!(topics.contains("Project.Investigate"));
        assert!(topics.contains("Project.Recall"));

        let error = workbench_doc("bogus", &modules).unwrap_err();
        assert!(error.contains("Project.Investigate"));
        assert!(error.contains("Project.Recall"));
    }
}
