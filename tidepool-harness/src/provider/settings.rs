//! The operator's live model/reasoning-effort dial: one shared,
//! durably-persisted [`ModelSettings`] handle. `tidepool-web`'s settings
//! route is the ONLY writer ([`SharedModelSettings::set`]); [`super::oauth`]'s
//! `OauthProvider` (built via `OauthProvider::with_live_settings`) is the
//! reader, at every request-build time — see [`super::oauth::OauthConfig`]'s
//! `tuning` doc for the full contract this establishes. This is
//! operator-initiated web-GUI config, not the `Ask`/`AskUser` operator-gate
//! machinery — it never rides `present_form`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::oauth::ReasoningEffort;

/// Known-supported Codex backend models
/// (`tidepool-harness/CLAUDE.md`'s backend canary section) — the fixed
/// allowlist a model name is validated against everywhere one enters the
/// system: the GUI dropdown, the durable settings file, and the settings
/// POST route. Never accepted as free text anywhere on this path.
pub const MODEL_ALLOWLIST: &[&str] = &["gpt-5.6-terra", "gpt-5.6-sol"];

/// Whether `model` is on [`MODEL_ALLOWLIST`] — the one check every entry
/// point (GUI render, settings-file load, POST route) shares.
pub fn is_allowed_model(model: &str) -> bool {
    MODEL_ALLOWLIST.contains(&model)
}

/// The two dial values. `effort` reuses [`ReasoningEffort`] — the same enum
/// [`super::oauth::ReasoningTuningArgs`] parses at boot — never a second,
/// parallel effort type. The `reasoning.summary` knob is NOT part of the
/// dial: it stays fixed at whatever [`super::oauth::ReasoningTuningArgs`]
/// resolved at boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub model: String,
    pub effort: ReasoningEffort,
}

impl ModelSettings {
    pub fn new(model: impl Into<String>, effort: ReasoningEffort) -> Self {
        Self {
            model: model.into(),
            effort,
        }
    }
}

/// The one shared, live-mutable settings handle: cheap to [`Clone`] (an
/// `Arc` clone), so the provider and the web `AppState` each hold their own
/// clone over the SAME underlying lock — a mutation through either clone is
/// visible to the other immediately.
#[derive(Clone)]
pub struct SharedModelSettings {
    inner: Arc<RwLock<ModelSettings>>,
    path: PathBuf,
}

impl SharedModelSettings {
    /// Load initial settings from `path` (a small `settings.json` beside the
    /// selfharness checkpoint —
    /// [`crate::selfharness::persistence::default_settings_path`]), falling
    /// back to `default` (the env/clap-resolved settings the binary would
    /// otherwise use) when the file is absent or fails to parse. A durable
    /// dial choice OUTRANKS a stale env default on restart: `default` is
    /// used only on the very first boot, before any dial change has ever
    /// been persisted.
    pub fn load_or(path: PathBuf, default: ModelSettings) -> Self {
        let initial = load_file(&path).unwrap_or(default);
        Self {
            inner: Arc::new(RwLock::new(initial)),
            path,
        }
    }

    pub fn get(&self) -> ModelSettings {
        self.inner.read().clone()
    }

    /// Mutate the live handle and persist to disk — the ONE write path (the
    /// web settings route is this crate's only caller). A write failure is
    /// reported to the caller rather than silently swallowed: a dial change
    /// that couldn't be persisted would silently revert on the next
    /// restart, which is worse than a visible error.
    pub fn set(&self, settings: ModelSettings) -> std::io::Result<()> {
        *self.inner.write() = settings.clone();
        save_file(&self.path, &settings)
    }
}

fn load_file(path: &Path) -> Option<ModelSettings> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_file(path: &Path, settings: &ModelSettings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)?;
    tidepool_atomic_write::write_durable(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tp-model-settings-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn is_allowed_model_checks_the_fixed_allowlist() {
        assert!(is_allowed_model("gpt-5.6-terra"));
        assert!(is_allowed_model("gpt-5.6-sol"));
        assert!(!is_allowed_model("gpt-4o-mini"));
        assert!(!is_allowed_model(""));
    }

    #[test]
    fn load_or_uses_default_when_file_absent() {
        let path = tempdir_path("absent").join("settings.json");
        let default = ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium);
        let handle = SharedModelSettings::load_or(path, default.clone());
        assert_eq!(handle.get(), default);
    }

    #[test]
    fn load_or_prefers_the_persisted_file_over_the_default() {
        let path = tempdir_path("present").join("settings.json");
        let persisted = ModelSettings::new("gpt-5.6-sol", ReasoningEffort::High);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let default = ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium);
        let handle = SharedModelSettings::load_or(path, default);
        assert_eq!(handle.get(), persisted);
    }

    #[test]
    fn set_persists_and_a_fresh_load_sees_the_new_value() {
        let path = tempdir_path("roundtrip").join("settings.json");
        let default = ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium);
        let handle = SharedModelSettings::load_or(path.clone(), default);

        let dialed = ModelSettings::new("gpt-5.6-sol", ReasoningEffort::High);
        handle.set(dialed.clone()).expect("set persists");
        assert_eq!(handle.get(), dialed);

        // A SEPARATE handle loading from the same path (simulating a
        // restart) must see the persisted value, not the original default.
        let reloaded = SharedModelSettings::load_or(
            path,
            ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
        );
        assert_eq!(reloaded.get(), dialed);
    }

    #[test]
    fn clones_share_the_same_underlying_state() {
        let path = tempdir_path("shared").join("settings.json");
        let default = ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium);
        let handle = SharedModelSettings::load_or(path, default);
        let clone = handle.clone();

        clone
            .set(ModelSettings::new("gpt-5.6-sol", ReasoningEffort::Low))
            .expect("set persists");
        assert_eq!(handle.get().model, "gpt-5.6-sol");
        assert_eq!(handle.get().effort, ReasoningEffort::Low);
    }
}
