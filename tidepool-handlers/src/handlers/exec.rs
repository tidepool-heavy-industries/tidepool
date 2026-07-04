use std::path::PathBuf;

// ============================================================================
// Tag 5: Exec (shell commands)
// ============================================================================

// ExecReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::exec_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

    fn resolve_dir(&self, rel: &str) -> Result<PathBuf, ExecError> {
        let target = self.root.join(rel);
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| ExecError::ExecBadDir(e.to_string()))?;
        let canonical = target
            .canonicalize()
            .map_err(|e| ExecError::ExecBadDir(format!("Cannot resolve directory: {}", e)))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(ExecError::ExecBadDir(format!(
                "Path escapes sandbox: {}",
                rel
            )));
        }
        Ok(canonical)
    }

    fn run_command(
        &self,
        cmd: &str,
        dir: &std::path::Path,
    ) -> Result<(i64, String, String), ExecError> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| ExecError::ExecSpawn(format!("exec failed: {}", e)))?;

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stdout.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stdout.is_char_boundary(end) {
                end -= 1;
            }
            stdout.truncate(end);
            stdout.push_str("\n...[truncated at 2MB]");
        }
        if stderr.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stderr.is_char_boundary(end) {
                end -= 1;
            }
            stderr.truncate(end);
            stderr.push_str("\n...[truncated at 2MB]");
        }
        let code = output.status.code().unwrap_or(-1) as i64;
        Ok((code, stdout, stderr))
    }
}

impl ExecHandler {
    // Errors-tagged verbs: total in `ExecError`, no `cx` — the dispatch arm
    // wraps the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335. A
    // nonzero EXIT is not a failure: `run_command` always returns `Ok((code,
    // out, err))` once the process spawns — `Err` is only ExecSpawn/ExecBadDir.
    fn exec_run(&mut self, cmd: String) -> Result<(i64, String, String), ExecError> {
        self.run_command(&cmd, &self.root.clone())
    }

    fn exec_run_in(&mut self, dir: String, cmd: String) -> Result<(i64, String, String), ExecError> {
        let target = self.resolve_dir(&dir)?;
        self.run_command(&cmd, &target)
    }

    fn exec_run_argv(&mut self, argv: Vec<String>) -> Result<(i64, String, String), ExecError> {
        if argv.is_empty() {
            return Err(ExecError::ExecSpawn("runArgv: empty argv".to_string()));
        }
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&self.root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| ExecError::ExecSpawn(format!("runArgv exec failed: {}", e)))?;
        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stdout.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stdout.is_char_boundary(end) {
                end -= 1;
            }
            stdout.truncate(end);
            stdout.push_str("\n...[truncated at 2MB]");
        }
        if stderr.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stderr.is_char_boundary(end) {
                end -= 1;
            }
            stderr.truncate(end);
            stderr.push_str("\n...[truncated at 2MB]");
        }
        let code = output.status.code().unwrap_or(-1) as i64;
        Ok((code, stdout, stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_eval::value::Value;

    #[test]
    fn test_exec_from_core_run() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("Run").unwrap();
        let cmd = "echo hello".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![cmd]);
        let req = ExecReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ExecReq::Run(ref c) if c == "echo hello"));
    }

    #[test]
    fn test_exec_from_core_run_in() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("RunIn").unwrap();
        let dir = "/tmp".to_string().to_value(&table).unwrap();
        let cmd = "ls".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![dir, cmd]);
        let req = ExecReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ExecReq::RunIn(ref d, ref c) if d == "/tmp" && c == "ls"));
    }

    /// #335 end-to-end acceptance: `runIn` with a bad/escaping directory is a
    /// typed `Left (ExecBadDir _)` the eval pattern-matches — never an abort.
    #[tokio::test]
    async fn exec_run_in_bad_dir_is_typed_left_execbaddir() {
        let v = jit_eval(&[
            "r <- runIn \"../../nope-335\" \"echo hi\"",
            "pure (case r of { Left (ExecBadDir _) -> (\"baddir\" :: Text); Left _ -> \"other\"; Right _ -> \"ok\" })",
        ]);
        assert_eq!(v, serde_json::json!("baddir"));
    }

    /// The happy path still threads through the Either: `run` on a valid
    /// command is `Right _`, so `run cmd >>= liftEither` (the natural unwrap)
    /// yields the Proc.
    #[tokio::test]
    async fn exec_run_existing_command_is_right() {
        let v = jit_eval(&["p <- run \"echo hi\" >>= liftEither", "pure (ok p)"]);
        assert_eq!(v, serde_json::json!(true));
    }
}
