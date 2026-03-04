# Plan 2: Cap Run/RunIn exec output to prevent OOM

## Problem

`tidepool/src/main.rs` — the `run_command()` helper (line 792-810) calls `std::process::Command::new("sh").arg("-c").arg(cmd)...output()` which buffers the entire stdout/stderr in memory. `RunJson` has a 512KB cap (`MAX_JSON_OUTPUT_BYTES` at line 790), but `Run` (line 827) and `RunIn` (line 853) pass stdout/stderr through uncapped. A single `run "find /"` or `run "yes"` can OOM the server.

## Files to modify

- `tidepool/src/main.rs` — `run_command()` method and the `MAX_JSON_OUTPUT_BYTES` constant area

## Implementation

### Step 1: Add output cap constant

Next to the existing constant at line 790:
```rust
const MAX_JSON_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024; // 2MB cap for Run/RunIn
```

### Step 2: Truncate in `run_command()`

After the `String::from_utf8_lossy` calls (lines 806-807), add truncation:

```rust
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
        stdout.truncate(Self::MAX_EXEC_OUTPUT_BYTES);
        stdout.push_str("\n...[truncated at 2MB]");
    }
    if stderr.len() > Self::MAX_EXEC_OUTPUT_BYTES {
        stderr.truncate(Self::MAX_EXEC_OUTPUT_BYTES);
        stderr.push_str("\n...[truncated at 2MB]");
    }
    let code = output.status.code().unwrap_or(-1) as i64;
    Ok((code, stdout, stderr))
}
```

Note: `MAX_EXEC_OUTPUT_BYTES` needs to be an associated const on the handler struct, or a module-level const. Currently `MAX_JSON_OUTPUT_BYTES` is defined at line 790 as `const MAX_JSON_OUTPUT_BYTES: usize = 512 * 1024;` — check if it's an associated const or module const and match the style.

### Step 3: Verify truncation is char-safe

`String::truncate` can panic if the index is not on a char boundary. Since `from_utf8_lossy` produces valid UTF-8, we need to find the nearest char boundary:

```rust
if stdout.len() > Self::MAX_EXEC_OUTPUT_BYTES {
    let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
    while !stdout.is_char_boundary(end) { end -= 1; }
    stdout.truncate(end);
    stdout.push_str("\n...[truncated at 2MB]");
}
```

## Verification

```bash
cargo test --workspace
```

MCP test: `run "seq 1 10000000"` should return truncated output, not OOM. `run "echo hello"` should work normally.
