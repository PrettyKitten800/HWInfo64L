use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Trusted system binary search paths (never consults untrusted `$PATH`).
const TRUSTED_BIN_DIRS: &[&str] = &["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

/// Resolve a system binary strictly inside trusted root-owned system directories,
/// verifying root ownership (`uid == 0`) and rejecting group/world-writable files.
pub(crate) fn resolve_trusted_binary(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\0') {
        return None;
    }
    for dir in TRUSTED_BIN_DIRS {
        let candidate = Path::new(dir).join(name);
        let Ok(canon) = fs::canonicalize(&candidate) else {
            continue;
        };
        let Ok(meta) = fs::metadata(&canon) else {
            continue;
        };
        let mode = meta.permissions().mode();
        if meta.is_file() && meta.uid() == 0 && (mode & 0o111) != 0 && (mode & 0o022) == 0 {
            return Some(canon);
        }
    }
    None
}

/// Read a sysfs/procfs file and trim trailing whitespace.
pub(crate) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse::<u64>().ok()
}

pub(crate) fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    read_trimmed(path)?.parse::<i64>().ok()
}

/// Sanitize an arbitrary label into a stable lowercase ASCII sensor ID segment.
pub(crate) fn sanitize_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_underscore = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// Check whether the current process is running as effective root (UID 0).
pub(crate) fn is_effective_root() -> bool {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let mut cols = rest.split_whitespace();
                let _real = cols.next();
                if let Some(effective) = cols.next() {
                    return effective == "0";
                }
            }
        }
    }
    false
}
