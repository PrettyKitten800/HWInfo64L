use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use hwinfo64l_protocol::{
    PluginFromHost, PluginManifest, PluginToHost, PollTier, PROTOCOL_VERSION,
};

use crate::probe::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};

/// Maximum bytes accepted in a single NDJSON line from an external plugin (512 KiB).
const MAX_PLUGIN_LINE_BYTES: usize = 512 * 1024;

/// Read a single `\n`-terminated line up to `max_bytes`, protecting against OOM
/// from a misbehaving plugin streaming unbounded bytes without newlines.
fn read_line_bounded<R: BufRead>(reader: &mut R, max_bytes: usize) -> Result<Option<String>, ProbeError> {
    let mut buf = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > max_bytes {
        return Err(ProbeError::Transient(format!(
            "plugin line exceeded maximum limit of {max_bytes} bytes"
        )));
    }
    let text = String::from_utf8(buf)
        .map_err(|e| ProbeError::Transient(format!("plugin emitted invalid UTF-8: {e}")))?;
    Ok(Some(text))
}

/// Discover executable plugin binaries/scripts in a plugin directory.
///
/// Supports either direct executables (`<dir>/my-plugin`) or one-level
/// subdirectories (`<dir>/my-plugin/plugin` or any single executable inside).
pub fn discover_plugin_executables(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_executable_file(&path) {
            found.push(path);
        } else if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) {
                for sub_entry in sub.flatten() {
                    let sub_path = sub_entry.path();
                    if is_executable_file(&sub_path) {
                        found.push(sub_path);
                        break;
                    }
                }
            }
        }
    }
    found.sort();
    found
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

struct RunningPlugin {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// Supervised external NDJSON plugin probe speaking protocol v1 over stdin/stdout.
pub struct NdjsonPluginProbe {
    id: String,
    display_name: String,
    executable: PathBuf,
    interval: Duration,
    manifest: Option<PluginManifest>,
    process: Option<RunningPlugin>,
}

impl NdjsonPluginProbe {
    pub fn new(executable: PathBuf) -> Self {
        let fallback_name = executable
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin")
            .to_string();
        Self {
            id: format!("plugin:{fallback_name}"),
            display_name: format!("Plugin ({fallback_name})"),
            executable,
            interval: Duration::from_secs(1),
            manifest: None,
            process: None,
        }
    }

    fn ensure_started(&mut self) -> Result<Option<PluginManifest>, ProbeError> {
        if self.process.is_some() {
            return Ok(None);
        }

        let mut child = Command::new(&self.executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProbeError::Transient("failed to capture plugin stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProbeError::Transient("failed to capture plugin stdout".to_string()))?;
        let mut reader = BufReader::new(stdout);

        let hello = serde_json::to_string(&PluginFromHost::Hello {
            protocol_version: PROTOCOL_VERSION,
        })
        .map_err(|e| ProbeError::Transient(e.to_string()))?;
        writeln!(stdin, "{hello}")?;
        stdin.flush()?;

        let Some(line) = read_line_bounded(&mut reader, MAX_PLUGIN_LINE_BYTES).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeError::Transient(
                "plugin exited before sending manifest".to_string(),
            ));
        };

        let msg: PluginToHost = serde_json::from_str(line.trim())
            .map_err(|e| ProbeError::Transient(format!("invalid plugin manifest JSON: {e}")))?;

        match msg {
            PluginToHost::Manifest(manifest) => {
                self.id = format!("plugin:{}", manifest.id);
                self.display_name = manifest.name.clone();
                let clamped_ms = manifest.interval_ms.clamp(250, 60_000);
                self.interval = Duration::from_millis(clamped_ms);
                self.manifest = Some(manifest.clone());
                self.process = Some(RunningPlugin {
                    child,
                    stdin,
                    stdout: reader,
                });
                Ok(Some(manifest))
            }
            PluginToHost::Error { message } => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ProbeError::Transient(message))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ProbeError::Transient(
                    "expected manifest as first plugin message".to_string(),
                ))
            }
        }
    }

    fn tear_down(&mut self) {
        if let Some(mut running) = self.process.take() {
            if let Ok(msg) = serde_json::to_string(&PluginFromHost::Shutdown) {
                let _ = writeln!(running.stdin, "{msg}");
                let _ = running.stdin.flush();
            }
            drop(running.stdin);
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }
}

impl Drop for NdjsonPluginProbe {
    fn drop(&mut self) {
        self.tear_down();
    }
}

impl Probe for NdjsonPluginProbe {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll_interval(&self) -> Duration {
        self.interval
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(3)
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let mut output = ProbeOutput::default();
        if let Some(manifest) = self.ensure_started()? {
            output.descriptors = manifest.sensors;
            output.inventory = manifest.inventory;
        }

        let Some(running) = self.process.as_mut() else {
            return Err(ProbeError::Transient("plugin process not running".to_string()));
        };

        let poll_msg = serde_json::to_string(&PluginFromHost::Poll)
            .map_err(|e| ProbeError::Transient(e.to_string()))?;
        if let Err(e) = writeln!(running.stdin, "{poll_msg}").and_then(|()| running.stdin.flush()) {
            self.tear_down();
            return Err(ProbeError::Io(e));
        }

        let line = match read_line_bounded(&mut running.stdout, MAX_PLUGIN_LINE_BYTES) {
            Ok(Some(l)) => l,
            Ok(None) => {
                self.tear_down();
                return Err(ProbeError::Transient("plugin closed stdout".to_string()));
            }
            Err(e) => {
                self.tear_down();
                return Err(e);
            }
        };

        let msg: PluginToHost = serde_json::from_str(line.trim())
            .map_err(|e| ProbeError::Transient(format!("invalid plugin reading JSON: {e}")))?;

        match msg {
            PluginToHost::Readings {
                timestamp_ms,
                values,
            } => {
                let ts = if timestamp_ms == 0 {
                    now_epoch_ms()
                } else {
                    timestamp_ms
                };
                for (id, value) in values {
                    output.samples.push(RawSample::new(id, value, ts));
                }
            }
            PluginToHost::Inventory { nodes } => {
                output.inventory.extend(nodes);
            }
            PluginToHost::Error { message } => {
                return Err(ProbeError::Transient(message));
            }
            PluginToHost::Manifest(manifest) => {
                output.descriptors = manifest.sensors;
                output.inventory = manifest.inventory;
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_plugin_reader_rejects_oversized_lines() {
        let huge = vec![b'a'; 1024];
        let mut cursor = Cursor::new(huge);
        let res = read_line_bounded(&mut cursor, 128);
        assert!(res.is_err());

        let mut ok_cursor = Cursor::new(b"{\"type\":\"poll\"}\n".to_vec());
        let ok = read_line_bounded(&mut ok_cursor, 128);
        assert!(matches!(ok, Ok(Some(_))));
    }
}
