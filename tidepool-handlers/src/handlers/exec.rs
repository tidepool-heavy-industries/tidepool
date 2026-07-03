use std::path::PathBuf;
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 5: Exec (shell commands)
// ============================================================================

#[derive(FromCore)]
pub enum ExecReq {
    #[core(name = "Run")]
    Run(String),
    #[core(name = "RunIn")]
    RunIn(String, String),
    #[core(name = "TryRun")]
    TryRun(String),
    #[core(name = "TryRunIn")]
    TryRunIn(String, String),
    #[core(name = "RunArgv")]
    RunArgv(Vec<String>),
}

#[derive(Clone)]
pub struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

    fn resolve_dir(&self, rel: &str) -> Result<PathBuf, EffectError> {
        let target = self.root.join(rel);
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;
        let canonical = target
            .canonicalize()
            .map_err(|e| EffectError::Handler(format!("Cannot resolve directory: {}", e)))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(EffectError::Handler(format!(
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
    ) -> Result<(i64, String, String), EffectError> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| EffectError::Handler(format!("exec failed: {}", e)))?;

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

impl DescribeEffect for ExecHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::exec_decl()
    }
}

impl EffectHandler<CapturedOutput> for ExecHandler {
    type Request = ExecReq;
    fn handle(
        &mut self,
        req: ExecReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ExecReq::Run(cmd) => {
                let (code, stdout, stderr) = self.run_command(&cmd, &self.root)?;
                cx.respond((code, stdout, stderr))
            }
            ExecReq::RunIn(dir, cmd) => {
                let target = self.resolve_dir(&dir)?;
                let (code, stdout, stderr) = self.run_command(&cmd, &target)?;
                cx.respond((code, stdout, stderr))
            }
            ExecReq::TryRun(cmd) => cx.respond_caught(self.run_command(&cmd, &self.root)),
            ExecReq::TryRunIn(dir, cmd) => cx.respond_caught(
                self.resolve_dir(&dir)
                    .and_then(|target| self.run_command(&cmd, &target)),
            ),
            ExecReq::RunArgv(argv) => {
                if argv.is_empty() {
                    return Err(EffectError::Handler("runArgv: empty argv".to_string()));
                }
                let output = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .current_dir(&self.root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .map_err(|e| EffectError::Handler(format!("runArgv exec failed: {}", e)))?;
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
                cx.respond((code, stdout, stderr))
            }
        }
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
}
