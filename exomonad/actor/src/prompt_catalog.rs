use std::borrow::Cow;

const HOSTED_DESCRIPTION_LIMIT: usize = 1024;
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
    utf8_char_count(HASKELL_TOOL_INSTRUCTIONS) <= HOSTED_DESCRIPTION_LIMIT,
    "hosted Haskell tool instructions exceed the provider limit"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptId {
    HaskellToolInstructions,
}

impl PromptId {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 1] = [Self::HaskellToolInstructions];

    pub(crate) fn artifact(self) -> PromptArtifact {
        match self {
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

/// Fingerprint of the shared hosted Haskell usage instructions. Per-tool
/// descriptions belong to the typed declarations supplied by the AgentSpec
/// and the actor-local builtins; this digest does not cover those declarations.
/// The composition root combines this with its base and agent prompt digest.
pub fn hosted_prompt_fingerprint() -> String {
    let mut hasher = blake3::Hasher::new();
    let body = PromptId::HaskellToolInstructions.body();
    hasher.update(&(body.len() as u64).to_le_bytes());
    hasher.update(body.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptRole {
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
const SHIPPED_SKILLS: [&str; 9] = [
    "exomonad-jev",
    "exomonad-agent-work",
    "exomonad-workbench",
    "exomonad-cleanup",
    "exomonad-project-work",
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
        "agents" | "agent-work" => Ok(Cow::Borrowed(include_str!(
            "../../prompts/docs/agents.md"
        ))),
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
                "Exomonad topics: tree (worktree), workbench, request, agents, watch, deadline, refinement, lineage, cleanup, recovery, jev, actors, reflect. Use hosted `lookup` with `doc <topic>`.\n\
                 Load the skill first where one exists; a topic is the fallback. Workspace skills: exomonad-workbench (Kleisli composition, optics and local languages), exomonad-define-actors (stateful interpreters for typed calls and events), exomonad-jev (semantic predicates, choices and continuations), exomonad-project-work (optional Git delivery workflow), exomonad-agent-work (typed agents, requests and waits), exomonad-agent-spec (model-facing tools and reloads), exomonad-command (commands as values and retained results), exomonad-review (exact-source review and repair), exomonad-cleanup (retiring actors and scoped resources).",
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
            // Route skill names and their short aliases to the shipped entrypoint.
            if let Some(skill) = skill_for_topic(other) {
                message.push_str(&format!(
                    "; the `{skill}` skill covers this — load it first, a topic is the fallback"
                ));
            }
            message.push_str(
                "; topics: tree (worktree), workbench, request, agents, watch, deadline, refinement, lineage, cleanup, recovery, jev, actors; `doc topics` lists the workspace skills"
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
            [PromptRole::HostedToolInstructions]
        );
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.body.chars().count() <= HOSTED_DESCRIPTION_LIMIT));
        for topic in [
            "tree",
            "workbench",
            "request",
            "agents",
            "watch",
            "deadline",
            "refinement",
            "lineage",
            "cleanup",
            "recovery",
            "jev",
            "actors",
        ] {
            assert!(!workbench_doc(topic, &[]).unwrap().trim().is_empty());
        }
        // Every topic with a workspace skill names it on its last line, and the
        // topic listing names the skills beside the topics. `jev` instead
        // links the skill inline and says so, rather than duplicating its
        // content behind a trailer.
        for (topic, skill) in [
            ("actors", "exomonad-define-actors"),
            ("cleanup", "exomonad-cleanup"),
            ("agents", "exomonad-agent-work"),
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
        assert!(workbench_doc("topics", &[])
            .unwrap()
            .contains("exomonad-jev"));
        assert_eq!(hosted_prompt_fingerprint().len(), 64);
        assert!(workbench_doc("missing", &[]).is_err());
    }

    #[test]
    fn discovery_names_only_existing_shipped_skills() {
        let bundle = std::env::var_os("EXOMONAD_WORKSPACE_GIT_BUNDLE")
            .expect("native test runner supplies the shipped workspace bundle");
        let record = std::env::var_os("EXOMONAD_WORKSPACE_GITLINK")
            .expect("native test runner supplies the exact workspace gitlink");
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
        let revision = record["revision"]
            .as_str()
            .expect("recorded workspace commit");
        let git = std::path::PathBuf::from(
            std::env::var_os("TIDEPOOL_WORKSPACE_TEST_GIT")
                .expect("native test runner supplies declared Git"),
        );
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let run = |arguments: &[&std::ffi::OsStr]| {
            let result = std::process::Command::new(&git)
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(arguments)
                .output()
                .expect("run declared Git on the shipped bundle");
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        run(&[
            "-c".as_ref(),
            "protocol.file.allow=always".as_ref(),
            "clone".as_ref(),
            "--quiet".as_ref(),
            "--no-checkout".as_ref(),
            bundle.as_ref(),
            workspace.as_os_str(),
        ]);
        run(&[
            "-C".as_ref(),
            workspace.as_os_str(),
            "checkout".as_ref(),
            "--quiet".as_ref(),
            "--detach".as_ref(),
            revision.as_ref(),
        ]);
        let root = workspace.join("skills");
        let mut installed = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                installed.insert(entry.file_name().into_string().unwrap());
            }
        }
        assert_eq!(
            installed,
            SHIPPED_SKILLS.into_iter().map(str::to_owned).collect(),
            "discovery must describe the skills in the actual pinned bundle",
        );
        for skill in SHIPPED_SKILLS {
            assert!(
                root.join(skill).join("SKILL.md").is_file(),
                "missing {skill}"
            );
        }
    }

    #[test]
    fn an_unknown_topic_naming_a_shipped_skill_says_so() {
        // A short skill alias resolves even when it has no built-in doc topic.
        let refusal = workbench_doc("command", &[]).unwrap_err();
        assert!(
            refusal.contains("`exomonad-command` skill covers this"),
            "{refusal}"
        );
        assert!(refusal.starts_with("unknown Exomonad documentation topic `command`"));

        // The full name works too, and so does every other shipped skill that
        // is not already a topic in its own right.
        for topic in [
            "exomonad-command",
            "review",
            "coordinate",
            "project-work",
            "exomonad-project-work",
        ] {
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
