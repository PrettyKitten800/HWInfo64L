use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::{read_trimmed, read_u64, sanitize_key};

#[derive(Debug, Clone, Copy)]
struct NicCounters {
    rx_bytes: u64,
    tx_bytes: u64,
    at: Instant,
}

/// Native `/sys/class/net` throughput probe (KB/s RX & TX) for active physical NICs.
pub struct NetworkProbe {
    prev: BTreeMap<String, NicCounters>,
}

impl NetworkProbe {
    pub fn new() -> Self {
        Self {
            prev: BTreeMap::new(),
        }
    }
}

impl Default for NetworkProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for NetworkProbe {
    fn id(&self) -> &str {
        "network"
    }

    fn display_name(&self) -> &str {
        "Network Interfaces"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let Ok(entries) = fs::read_dir("/sys/class/net") else {
            return Err(ProbeError::Unsupported("/sys/class/net missing".into()));
        };

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();
        let now = Instant::now();

        let mut ifaces: Vec<_> = entries.flatten().collect();
        ifaces.sort_by_key(|e| e.file_name());

        for entry in ifaces {
            let iface = entry.file_name().to_string_lossy().to_string();
            if iface == "lo"
                || iface.starts_with("docker")
                || iface.starts_with("veth")
                || iface.starts_with("br-")
            {
                continue;
            }
            let path = entry.path();
            let oper = read_trimmed(path.join("operstate")).unwrap_or_default();
            if oper != "up" {
                continue;
            }
            let stats = Path::new(&path).join("statistics");
            let Some(rx) = read_u64(stats.join("rx_bytes")) else {
                continue;
            };
            let Some(tx) = read_u64(stats.join("tx_bytes")) else {
                continue;
            };

            let (rx_kbs, tx_kbs) = if let Some(prev) = self.prev.get(&iface) {
                let dt = now.duration_since(prev.at).as_secs_f64();
                if dt > 0.05 {
                    let drx = rx.saturating_sub(prev.rx_bytes) as f64 / 1024.0 / dt;
                    let dtx = tx.saturating_sub(prev.tx_bytes) as f64 / 1024.0 / dt;
                    (drx, dtx)
                } else {
                    (0.0, 0.0)
                }
            } else {
                (0.0, 0.0)
            };

            self.prev.insert(
                iface.clone(),
                NicCounters {
                    rx_bytes: rx,
                    tx_bytes: tx,
                    at: now,
                },
            );

            let key = sanitize_key(&iface);
            let group = format!("Network ({iface})");
            for (suffix, label, val) in [
                ("rx", "Download Rate", rx_kbs),
                ("tx", "Upload Rate", tx_kbs),
            ] {
                let id = format!("net.{key}.rate.{suffix}");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: label.into(),
                    kind: SensorKind::Throughput,
                    unit: "KB/s".into(),
                    min_expected: Some(0.0),
                    max_expected: None,
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
