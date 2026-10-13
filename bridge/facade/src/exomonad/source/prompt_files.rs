//! Workspace-authored instruction files captured before prompt composition.
use super::*;
use tidepool_runtime::session::ComposedPromptSnapshot;

/// Authored instruction roles captured before candidate composition.
#[derive(Clone, Debug, Default)]
pub(crate) struct CapturedPromptFiles {
    pub(super) files: BTreeMap<String, String>,
}

impl CapturedPromptFiles {
    pub(crate) fn from_frozen(inputs: Option<&FrozenWorkspace>) -> Self {
        Self { files: inputs.map_or_else(BTreeMap::new, |inputs| inputs.prompts.clone()) }
    }
    pub(crate) fn base(&self) -> Option<&str> { self.files.get("base").map(String::as_str) }
    pub(crate) fn agent(&self) -> Option<&str> { self.files.get("agent").map(String::as_str) }
    pub(crate) fn api_guide(&self) -> Option<&str> { self.files.get("api-guide").map(String::as_str) }
}

pub(super) type PromptComposer = Arc<dyn Fn(&CapturedPromptFiles) -> std::result::Result<ComposedPromptSnapshot, String> + Send + Sync>;

impl ExomonadSourceReload {
    pub(crate) fn set_prompt_composer(&self, actor: PrincipalId, composer: PromptComposer) -> std::result::Result<(), String> {
        let mut composers = self.prompt_composers.write();
        match composers.entry(actor) {
            std::collections::hash_map::Entry::Vacant(slot) => { slot.insert(composer); Ok(()) },
            std::collections::hash_map::Entry::Occupied(_) => Err("actor prompt compositor was already installed".into()),
        }
    }

}
