use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::read_u64;

#[derive(Debug, Clone, Copy, Default)]
struct CpuTimes {
    busy: u64,
    total: u64,
}

/// Native `/proc/stat` + `/sys/devices/system/cpu/cpu*/cpufreq` probe
/// reporting total/per-core utilization (%) and live per-core/average clocks (MHz).
pub struct CpuTelemetryProbe {
    prev_times: BTreeMap<String, CpuTimes>,
}

impl CpuTelemetryProbe {
    pub fn new() -> Self {
        Self {
            prev_times: BTreeMap::new(),
        }
    }
}

impl Default for CpuTelemetryProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for CpuTelemetryProbe {
    fn id(&self) -> &str {
        "cpu_telemetry"
    }

    fn display_name(&self) -> &str {
        "CPU Clocks & Utilization"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        // 1. CPU Utilization from /proc/stat
        if let Ok(stat) = fs::read_to_string("/proc/stat") {
            for line in stat.lines() {
                if !line.starts_with("cpu") {
                    continue;
                }
                let mut cols = line.split_whitespace();
                let Some(label) = cols.next() else {
                    continue;
                };
                let nums: Vec<u64> = cols.filter_map(|s| s.parse::<u64>().ok()).collect();
                if nums.len() < 4 {
                    continue;
                }
                let user = nums.first().copied().unwrap_or(0);
                let nice = nums.get(1).copied().unwrap_or(0);
                let system = nums.get(2).copied().unwrap_or(0);
                let idle = nums.get(3).copied().unwrap_or(0);
                let iowait = nums.get(4).copied().unwrap_or(0);
                let irq = nums.get(5).copied().unwrap_or(0);
                let softirq = nums.get(6).copied().unwrap_or(0);
                let steal = nums.get(7).copied().unwrap_or(0);

                let idle_all = idle.saturating_add(iowait);
                let busy = user
                    .saturating_add(nice)
                    .saturating_add(system)
                    .saturating_add(irq)
                    .saturating_add(softirq)
                    .saturating_add(steal);
                let total = busy.saturating_add(idle_all);
                let cur = CpuTimes { busy, total };

                let pct = if let Some(prev) = self.prev_times.get(label) {
                    let dt = cur.total.saturating_sub(prev.total);
                    let db = cur.busy.saturating_sub(prev.busy);
                    if dt > 0 {
                        (db as f64 / dt as f64) * 100.0
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                self.prev_times.insert(label.to_string(), cur);

                let (id, name) = if label == "cpu" {
                    (
                        "cpustat.system.util.total".to_string(),
                        "Total CPU Usage".to_string(),
                    )
                } else {
                    let idx = label.trim_start_matches("cpu");
                    (
                        format!("cpustat.{label}.util.usage"),
                        format!("Core #{idx} Usage"),
                    )
                };

                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: "CPU Utilization".into(),
                    name,
                    kind: SensorKind::Utilization,
                    unit: "%".into(),
                    min_expected: Some(0.0),
                    max_expected: Some(100.0),
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, pct, ts));
            }
        }

        // 2. Per-core clocks from cpufreq sysfs
        let cpu_root = Path::new("/sys/devices/system/cpu");
        let mut core_mhz: Vec<(u32, f64)> = Vec::new();
        if let Ok(entries) = fs::read_dir(cpu_root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(idx_str) = name.strip_prefix("cpu") else {
                    continue;
                };
                let Ok(idx) = idx_str.parse::<u32>() else {
                    continue;
                };
                let freq_path = entry.path().join("cpufreq/scaling_cur_freq");
                if let Some(khz) = read_u64(freq_path) {
                    core_mhz.push((idx, (khz as f64) / 1000.0));
                }
            }
        }
        core_mhz.sort_by_key(|(idx, _)| *idx);

        if !core_mhz.is_empty() {
            let sum: f64 = core_mhz.iter().map(|(_, mhz)| *mhz).sum();
            let avg = sum / (core_mhz.len() as f64);
            let max_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
                .map(|khz| (khz as f64) / 1000.0);

            out.descriptors.push(SensorDescriptor {
                id: "cpufreq.system.clock.average".into(),
                group: "CPU Clocks".into(),
                name: "Average Effective Clock".into(),
                kind: SensorKind::Clock,
                unit: "MHz".into(),
                min_expected: Some(0.0),
                max_expected: max_mhz,
                warn_threshold: None,
                crit_threshold: None,
                poll_tier: PollTier::Fast,
                source: self.id().into(),
            });
            out.samples
                .push(RawSample::new("cpufreq.system.clock.average", avg, ts));

            for (idx, mhz) in core_mhz {
                let id = format!("cpufreq.cpu{idx}.clock.current");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: "CPU Clocks".into(),
                    name: format!("Core #{idx} Clock"),
                    kind: SensorKind::Clock,
                    unit: "MHz".into(),
                    min_expected: Some(0.0),
                    max_expected: max_mhz,
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, mhz, ts));
            }
        }

        Ok(out)
    }
}
