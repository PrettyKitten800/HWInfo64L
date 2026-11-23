use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::Instant;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::sanitize_key;

#[derive(Debug, Clone, Copy)]
struct DiskCounters {
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
    io_ms: u64,
    at: Instant,
}

/// Native `/proc/diskstats` throughput (MB/s), IOPS, and activity (%) probe
/// for physical block drives (`sd*`, `nvme*`, `mmcblk*`).
pub struct DiskIoProbe {
    prev: BTreeMap<String, DiskCounters>,
}

impl DiskIoProbe {
    pub fn new() -> Self {
        Self {
            prev: BTreeMap::new(),
        }
    }
}

impl Default for DiskIoProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for DiskIoProbe {
    fn id(&self) -> &str {
        "disk_io"
    }

    fn display_name(&self) -> &str {
        "Drive Throughput & Activity"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let physical = discover_physical_block_names();
        if physical.is_empty() {
            return Err(ProbeError::Unsupported("no physical block devices".into()));
        }

        let content = fs::read_to_string("/proc/diskstats")?;
        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();
        let now = Instant::now();

        for line in content.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 13 {
                continue;
            }
            let dev = cols[2];
            if !physical.contains(dev) {
                continue;
            }

            let reads = cols[3].parse::<u64>().unwrap_or(0);
            let sectors_read = cols[5].parse::<u64>().unwrap_or(0);
            let writes = cols[7].parse::<u64>().unwrap_or(0);
            let sectors_written = cols[9].parse::<u64>().unwrap_or(0);
            let io_ms = cols[12].parse::<u64>().unwrap_or(0);

            let (read_mbs, write_mbs, read_iops, write_iops, busy_pct) =
                if let Some(prev) = self.prev.get(dev) {
                    let dt = now.duration_since(prev.at).as_secs_f64();
                    if dt > 0.05 {
                        let dr_sec = sectors_read.saturating_sub(prev.sectors_read) as f64;
                        let dw_sec = sectors_written.saturating_sub(prev.sectors_written) as f64;
                        let dr_ops = reads.saturating_sub(prev.reads_completed) as f64;
                        let dw_ops = writes.saturating_sub(prev.writes_completed) as f64;
                        let dio = io_ms.saturating_sub(prev.io_ms) as f64;

                        let r_mbs = (dr_sec * 512.0) / (1024.0 * 1024.0) / dt;
                        let w_mbs = (dw_sec * 512.0) / (1024.0 * 1024.0) / dt;
                        let r_iops = dr_ops / dt;
                        let w_iops = dw_ops / dt;
                        let busy = ((dio / (dt * 1000.0)) * 100.0).clamp(0.0, 100.0);
                        (r_mbs, w_mbs, r_iops, w_iops, busy)
                    } else {
                        (0.0, 0.0, 0.0, 0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0, 0.0, 0.0, 0.0)
                };

            self.prev.insert(
                dev.to_string(),
                DiskCounters {
                    reads_completed: reads,
                    sectors_read,
                    writes_completed: writes,
                    sectors_written,
                    io_ms,
                    at: now,
                },
            );

            let key = sanitize_key(dev);
            let group = format!("Drive Activity (/dev/{dev})");
            let items = [
                ("read_mbs", "Read Rate", SensorKind::Throughput, "MB/s", read_mbs, None),
                ("write_mbs", "Write Rate", SensorKind::Throughput, "MB/s", write_mbs, None),
                ("read_iops", "Read IOPS", SensorKind::Counter, "IOPS", read_iops, None),
                ("write_iops", "Write IOPS", SensorKind::Counter, "IOPS", write_iops, None),
                ("activity", "Drive Activity", SensorKind::Utilization, "%", busy_pct, Some(100.0)),
            ];

            for (metric, label, kind, unit, val, max_exp) in items {
                let id = format!("disk.{key}.io.{metric}");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: label.into(),
                    kind,
                    unit: unit.into(),
                    min_expected: Some(0.0),
                    max_expected: max_exp,
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, val, ts));
            }
        }

        Ok(out)
    }
}

fn discover_physical_block_names() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return set;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("sd") || name.starts_with("nvme") || name.starts_with("mmcblk") {
            set.insert(name);
        }
    }
    set
}
