//! A tiny "is this a readable, executable regular file" check.
//!
//! Used by this crate's binary resolver for strict executable overrides.
//! This module owns the file-permission check; callers own platform policy.

use std::path::Path;

/// On Unix, verify `path` names a regular file this process can actually
/// READ, with at least one EXECUTE permission bit set — the two properties a
/// "not a readable file" error message promises but `is_file` alone never
/// checked, so a chmod-000 (or non-executable) override used to resolve
/// successfully and only fail later, as an opaque OS error, at spawn.
///
/// `File::open` is the real read-access check (it honors the same
/// permission/ACL evaluation a later spawn's read of the binary would hit);
/// the execute-bit check on `mode()` is the accessible without-`libc`
/// approximation of "executable" a portable boundary crate can perform — the same
/// thing a later spawn ultimately depends on to succeed.
#[cfg(unix)]
pub fn is_readable_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !path.is_file() || std::fs::File::open(path).is_err() {
        return false;
    }
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Off Unix, there is no portable, dependency-free access check this crate
/// crate can perform; `is_file` is what this precedence step has always
/// checked here, and a genuinely unusable binary still fails loudly at spawn
/// time.
#[cfg(not(unix))]
pub fn is_readable_executable_file(path: &Path) -> bool {
    path.is_file()
}
