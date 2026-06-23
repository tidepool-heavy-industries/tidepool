//! Layered configuration: built-in defaults < global `config.toml` <
//! project `config.toml` < environment.
//!
//! Env vars (`TIDEPOOL_*`) stay authoritative — they're read at their existing
//! sites and at resolution time. This only fills the gaps from on-disk config
//! files: a user-global `config.toml` (`~/.config/tidepool/config.toml`) and a
//! per-project one (`<project>/.tidepool/config.toml`), with project winning.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default LLM model for the `Llm` effect. Overridden by `--llm` /
    /// `TIDEPOOL_LLM_MODEL`.
    pub llm_model: Option<String>,
    /// Default eval window in seconds. Overridden by the per-request
    /// `timeout_secs` knob and `TIDEPOOL_EVAL_TIMEOUT_SECS`.
    pub eval_timeout_secs: Option<u64>,
}

impl Config {
    /// Load + merge the global then project `config.toml` (project keys win).
    /// Missing files are ignored; malformed ones are skipped with a warning.
    pub fn load(project_root: Option<&Path>) -> Self {
        let mut cfg = Config::default();
        cfg.merge_file(&tidepool_runtime::paths::config_dir().join("config.toml"));
        if let Some(root) = project_root {
            cfg.merge_file(&root.join(".tidepool").join("config.toml"));
        }
        cfg
    }

    fn merge_file(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return; // absent → nothing to merge
        };
        match toml::from_str::<Config>(&text) {
            Ok(other) => self.merge(other),
            Err(e) => tracing::warn!("ignoring malformed {}: {e}", path.display()),
        }
    }

    fn merge(&mut self, other: Config) {
        if other.llm_model.is_some() {
            self.llm_model = other.llm_model;
        }
        if other.eval_timeout_secs.is_some() {
            self.eval_timeout_secs = other.eval_timeout_secs;
        }
    }
}
