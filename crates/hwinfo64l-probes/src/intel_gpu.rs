use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};
use serde_json::Value;

use crate::util::{is_effective_root, read_trimmed, sanitize_key};

#[derive(Debug, Default, Clone)]
struct IntelGpuSnapshot {
    req_mhz: Option<f64>,
    act_mhz: Option<f64>,
    rc6_pct: Option<f64>,
    irq_s: Option<f64>,
    gpu_watts: Option<f64>,
    engines: BTreeMap<String, f64>,
    timestamp_ms: u64,
}

/// Streaming Intel iGPU engine probe backed by a single persistent
/// `intel_gpu_top -J -s 1000` worker thread (zero per-poll spawn overhead).
pub struct IntelGpuTopProbe {
    has_intel_gpu: bool,
    latest: Arc<Mutex<Option<IntelGpuSnapshot>>>,
    child: Option<Child>,
    started: bool,
}

impl IntelGpuTopProbe {
    pub fn new() -> Self {
        Self {
            has_intel_gpu: detect_intel_gpu(),
            latest: Arc::new(Mutex::new(None)),
            child: None,
            started: false,
        }
    }

    fn ensure_started(&mut self) -> Result<(), ProbeError> {
        if self.started {
            if let Some(ref mut c) = self.child {
                if let Ok(Some(_)) = c.try_wait() {
                    self.started = false;
                    self.child = None;
                } else {
                    return Ok(());
                }
            }
        }

        let mut child = match Command::new("intel_gpu_top")
            .args(["-J", "-s", "1000"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProbeError::Unsupported(
                    "intel_gpu_top not installed".into(),
                ));
            }
            Err(e) => return Err(ProbeError::Io(e)),
        };

        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeError::Transient(
                "failed to capture intel_gpu_top stdout".into(),
            ));
        };

        let latest = Arc::clone(&self.latest);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let mut obj_buf = String::new();
            let mut depth = 0i32;

            for line_res in reader.lines() {
                let Ok(line) = line_res else {
                    break;
                };
                for ch in line.chars() {
                    if ch == '{' {
                        depth += 1;
                    }
                    if depth > 0 {
                        obj_buf.push(ch);
                    }
                    if ch == '}' {
                        depth -= 1;
                        if depth == 0 && !obj_buf.is_empty() {
                            if let Ok(val) = serde_json::from_str::<Value>(&obj_buf) {
                                let snap = parse_gpu_json(&val);
                                if let Ok(mut guard) = latest.lock() {
                                    *guard = Some(snap);
                                }
                            }
                            obj_buf.clear();
                        }
                    }
                }
                if depth > 0 {
                    obj_buf.push('\n');
                }
            }
        });

        self.child = Some(child);
        self.started = true;
        Ok(())
    }
}

impl Default for IntelGpuTopProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IntelGpuTopProbe {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Probe for IntelGpuTopProbe {
    fn id(&self) -> &str {
        "intel_gpu_top"
    }

    fn display_name(&self) -> &str {
        "Intel iGPU Engines & RC6"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        if !self.has_intel_gpu {
            return Err(ProbeError::Unsupported("no Intel GPU detected".into()));
        }
        if !is_effective_root() {
            return Err(ProbeError::RequiresPrivilege(
                "intel_gpu_top PMU requires hwinfo64ld daemon (CAP_SYS_ADMIN)".into(),
            ));
        }

        self.ensure_started()?;

        let maybe_snap = self.latest.lock().ok().and_then(|g| g.clone());
        let Some(snap) = maybe_snap else {
            return Ok(ProbeOutput::default());
        };

        let mut out = ProbeOutput::default();
        let ts = snap.timestamp_ms;
        let group = "Intel iGPU Engines".to_string();

        if let Some(mhz) = snap.act_mhz {
            push_sensor(
                &mut out,
                self.id(),
                "igpu.intel.clock.actual",
                &group,
                "GPU Actual Clock",
                SensorKind::Clock,
                "MHz",
                mhz,
                None,
                ts,
            );
        }
        if let Some(mhz) = snap.req_mhz {
            push_sensor(
                &mut out,
                self.id(),
                "igpu.intel.clock.requested",
                &group,
                "GPU Requested Clock",
                SensorKind::Clock,
                "MHz",
                mhz,
                None,
                ts,
            );
        }
        if let Some(rc6) = snap.rc6_pct {
            push_sensor(
                &mut out,
                self.id(),
                "igpu.intel.util.rc6_idle",
                &group,
                "GPU RC6 Sleep Residency",
                SensorKind::Utilization,
                "%",
                rc6.clamp(0.0, 100.0),
                Some(100.0),
                ts,
            );
        }
        if let Some(irqs) = snap.irq_s {
            push_sensor(
                &mut out,
                self.id(),
                "igpu.intel.counter.interrupts",
                &group,
                "GPU Interrupts",
                SensorKind::Counter,
                "irq/s",
                irqs,
                None,
                ts,
            );
        }
        if let Some(w) = snap.gpu_watts {
            push_sensor(
                &mut out,
                self.id(),
                "igpu.intel.power.gpu",
                &group,
                "GPU Power (PMU)",
                SensorKind::Power,
                "W",
                w,
                None,
                ts,
            );
        }

        for (eng_name, busy) in snap.engines {
            let key = sanitize_key(&eng_name);
            push_sensor(
                &mut out,
                self.id(),
                &format!("igpu.intel.engine.{key}"),
                &group,
                &format!("Engine: {eng_name}"),
                SensorKind::Utilization,
                "%",
                busy.clamp(0.0, 100.0),
                Some(100.0),
                ts,
            );
        }

        Ok(out)
    }
}

fn detect_intel_gpu() -> bool {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return false;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        if read_trimmed(e.path().join("device/vendor")).as_deref() == Some("0x8086") {
            return true;
        }
    }
    false
}

fn parse_gpu_json(val: &Value) -> IntelGpuSnapshot {
    let mut snap = IntelGpuSnapshot {
        timestamp_ms: now_epoch_ms(),
        ..IntelGpuSnapshot::default()
    };

    if let Some(freq) = val.get("frequency") {
        snap.req_mhz = freq.get("requested").and_then(Value::as_f64);
        snap.act_mhz = freq.get("actual").and_then(Value::as_f64);
    }
    snap.rc6_pct = val
        .get("rc6")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_f64);
    snap.irq_s = val
        .get("interrupts")
        .and_then(|i| i.get("count"))
        .and_then(Value::as_f64);
    snap.gpu_watts = val
        .get("power")
        .and_then(|p| p.get("GPU"))
        .and_then(Value::as_f64);

    if let Some(engines) = val.get("engines").and_then(Value::as_object) {
        for (name, obj) in engines {
            if let Some(busy) = obj.get("busy").and_then(Value::as_f64) {
                snap.engines.insert(name.clone(), busy);
            }
        }
    }

    snap
}

#[allow(clippy::too_many_arguments)]
fn push_sensor(
    out: &mut ProbeOutput,
    source: &str,
    id: &str,
    group: &str,
    name: &str,
    kind: SensorKind,
    unit: &str,
    value: f64,
    max_expected: Option<f64>,
    ts: u64,
) {
    out.descriptors.push(SensorDescriptor {
        id: id.to_string(),
        group: group.to_string(),
        name: name.to_string(),
        kind,
        unit: unit.to_string(),
        min_expected: Some(0.0),
        max_expected,
        warn_threshold: None,
        crit_threshold: None,
        poll_tier: PollTier::Fast,
        source: source.to_string(),
    });
    out.samples.push(RawSample::new(id, value, ts));
}
