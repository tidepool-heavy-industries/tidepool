//! Config-dir resolution, mirroring `tidepool_runtime::paths::config_dir` /
//! `global_secrets_dirs` (env overrides: `TIDEPOOL_CONFIG_DIR` →
//! `XDG_CONFIG_HOME/tidepool` → `~/.config/tidepool` → a tmp fallback).
//! Duplicated rather than depended-on: segment 60 must not pull
//! `tidepool-runtime` into `tidepool-harness` just for path resolution
//! (00-scaffold's crate-boundary contract).

use std::path::PathBuf;

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// User-global config root: `$TIDEPOOL_CONFIG_DIR` → `$XDG_CONFIG_HOME/tidepool`
/// → `~/.config/tidepool` → `$TMPDIR/tidepool-config` (last resort).
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TIDEPOOL_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("tidepool");
    }
    if let Some(h) = home() {
        return h.join(".config").join("tidepool");
    }
    std::env::temp_dir().join("tidepool-config")
}

/// Secrets dir under the config root: `~/.config/tidepool/secrets/`.
pub fn secrets_dir() -> PathBuf {
    config_dir().join("secrets")
}

/// Write `contents` to `path` with mode 0600, creating parent dirs as
/// needed. Uses `OpenOptions::mode` at creation time so the file is never
/// briefly world-readable between create and chmod.
pub fn write_secret(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_honors_override() {
        let tmp = std::env::temp_dir().join(format!("tp-harness-paths-{}", std::process::id()));
        std::env::set_var("TIDEPOOL_CONFIG_DIR", &tmp);
        assert_eq!(config_dir(), tmp);
        assert_eq!(secrets_dir(), tmp.join("secrets"));
        std::env::remove_var("TIDEPOOL_CONFIG_DIR");
    }

    #[test]
    fn write_secret_sets_0600() {
        let dir = std::env::temp_dir().join(format!("tp-harness-secret-{}", std::process::id()));
        let path = dir.join("token.json");
        write_secret(&path, "{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
