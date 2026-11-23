use std::fs;
use std::process::Command;
use std::time::Duration;

use hwinfo64l_core::probe::SummaryPatch;
use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};
use serde_json::Value;

use crate::util::{is_effective_root, resolve_trusted_binary, sanitize_key};

/// Slow-tier SMART drive health & temperature probe via `smartctl -a -j`.
pub struct SmartProbe;

impl SmartProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SmartProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for SmartProbe {
    fn id(&self) -> &str {
        "smart"
    }

    fn display_name(&self) -> &str {
        "Drive SMART Health & Temps"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Slow
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        if !is_effective_root() {
            return Err(ProbeError::RequiresPrivilege(
                "SMART telemetry requires hwinfo64ld daemon (CAP_SYS_ADMIN)".into(),
            ));
        }

        let Some(bin) = resolve_trusted_binary("smartctl") else {
            return Err(ProbeError::Unsupported("smartctl not installed".into()));
        };

        let Ok(entries) = fs::read_dir("/sys/block") else {
            return Err(ProbeError::Unsupported("/sys/block missing".into()));
        };

        let mut drives: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|d| d.starts_with("sd") || d.starts_with("nvme"))
            .collect();
        drives.sort();

        if drives.is_empty() {
            return Err(ProbeError::Unsupported("no SMART-capable drives found".into()));
        }

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        for dev in drives {
            if !dev.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let dev_path = format!("/dev/{dev}");
            let output = match Command::new(&bin).args(["-a", "-j", "--", &dev_path]).output() {
                Ok(o) => o,
                Err(_) => continue,
            };

            let Ok(json) = serde_json::from_slice::<Value>(&output.stdout) else {
                continue;
            };

            let model = json
                .get("model_name")
                .and_then(Value::as_str)
                .unwrap_or(&dev);
            let group = format!("Drive ({dev}: {model})");
            let key = sanitize_key(&dev);

            if let Some(passed) = json
                .get("smart_status")
                .and_then(|s| s.get("passed"))
                .and_then(Value::as_bool)
            {
                let health_str = if passed { "PASSED" } else { "FAILED" }.to_string();
                out.summary_patch = Some(SummaryPatch::DriveSmartHealth {
                    device: dev.clone(),
                    health: health_str,
                });
            }

            if let Some(temp) = json
                .get("temperature")
                .and_then(|t| t.get("current"))
                .and_then(Value::as_f64)
            {
                let id = format!("smart.{key}.temp.drive");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "Drive Temperature".into(),
                    kind: SensorKind::Temperature,
                    unit: "°C".into(),
                    min_expected: Some(0.0),
                    max_expected: Some(75.0),
                    warn_threshold: Some(60.0),
                    crit_threshold: Some(70.0),
                    poll_tier: PollTier::Slow,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, temp, ts));
            }

            if let Some(hours) = json
                .get("power_on_time")
                .and_then(|t| t.get("hours"))
                .and_then(Value::as_f64)
            {
                let id = format!("smart.{key}.counter.power_on_hours");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "Power-On Hours".into(),
                    kind: SensorKind::Counter,
                    unit: "hrs".into(),
                    min_expected: Some(0.0),
                    max_expected: None,
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Slow,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, hours, ts));
            }

            // NVMe percentage used (wear)
            if let Some(wear) = json
                .get("nvme_smart_health_information_log")
                .and_then(|l| l.get("percentage_used"))
                .and_then(Value::as_f64)
            {
                let id = format!("smart.{key}.util.wear");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "SSD Wear Used".into(),
                    kind: SensorKind::Utilization,
                    unit: "%".into(),
                    min_expected: Some(0.0),
                    max_expected: Some(100.0),
                    warn_threshold: Some(80.0),
                    crit_threshold: Some(95.0),
                    poll_tier: PollTier::Slow,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, wear, ts));
            }
        }

        Ok(out)
    }
}
