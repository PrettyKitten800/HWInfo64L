use std::collections::BTreeMap;
use std::fs;

use hwinfo64l_core::probe::SummaryPatch;
use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::{read_trimmed, read_u64};

/// Native `/proc/meminfo` + Linux `zswap` telemetry probe.
pub struct MemoryProbe;

impl MemoryProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for MemoryProbe {
    fn id(&self) -> &str {
        "memory"
    }

    fn display_name(&self) -> &str {
        "System Memory & Zswap"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let content = fs::read_to_string("/proc/meminfo")?;
        let mut kb: BTreeMap<String, u64> = BTreeMap::new();
        for line in content.lines() {
            let Some((k, rest)) = line.split_once(':') else {
                continue;
            };
            if let Some(num_str) = rest.split_whitespace().next() {
                if let Ok(val) = num_str.parse::<u64>() {
                    kb.insert(k.trim().to_string(), val);
                }
            }
        }

        let total_kb = kb.get("MemTotal").copied().unwrap_or(0);
        let avail_kb = kb.get("MemAvailable").copied().unwrap_or(0);
        let used_kb = total_kb.saturating_sub(avail_kb);
        let swap_total_kb = kb.get("SwapTotal").copied().unwrap_or(0);
        let swap_free_kb = kb.get("SwapFree").copied().unwrap_or(0);
        let swap_used_kb = swap_total_kb.saturating_sub(swap_free_kb);
        let zswap_kb = kb.get("Zswap").copied().unwrap_or(0);
        let zswapped_kb = kb.get("Zswapped").copied().unwrap_or(0);

        let load_pct = if total_kb > 0 {
            (used_kb as f64 / total_kb as f64) * 100.0
        } else {
            0.0
        };

        let zswap_enabled = read_trimmed("/sys/module/zswap/parameters/enabled")
            .map(|s| s == "Y" || s == "1")
            .unwrap_or(false);
        let zswap_compressor = read_trimmed("/sys/module/zswap/parameters/compressor");

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();
        let total_mib = (total_kb as f64) / 1024.0;
        let swap_total_mib = (swap_total_kb as f64) / 1024.0;

        let metrics = [
            (
                "memory.system.util.load",
                "Physical Memory Load",
                SensorKind::Utilization,
                "%",
                load_pct,
                Some(100.0),
                Some(85.0),
                Some(95.0),
            ),
            (
                "memory.system.size.used",
                "Physical Memory Used",
                SensorKind::Memory,
                "MiB",
                (used_kb as f64) / 1024.0,
                Some(total_mib),
                None,
                None,
            ),
            (
                "memory.system.size.available",
                "Physical Memory Available",
                SensorKind::Memory,
                "MiB",
                (avail_kb as f64) / 1024.0,
                Some(total_mib),
                None,
                None,
            ),
            (
                "memory.system.size.swap_used",
                "Swap Used",
                SensorKind::Memory,
                "MiB",
                (swap_used_kb as f64) / 1024.0,
                Some(swap_total_mib),
                None,
                None,
            ),
        ];

        for (id, name, kind, unit, val, max_exp, warn, crit) in metrics {
            out.descriptors.push(SensorDescriptor {
                id: id.into(),
                group: "Memory".into(),
                name: name.into(),
                kind,
                unit: unit.into(),
                min_expected: Some(0.0),
                max_expected: max_exp,
                warn_threshold: warn,
                crit_threshold: crit,
                poll_tier: PollTier::Fast,
                source: self.id().into(),
            });
            out.samples.push(RawSample::new(id, val, ts));
        }

        if zswap_enabled {
            let compressed_mib = (zswap_kb as f64) / 1024.0;
            let orig_mib = (zswapped_kb as f64) / 1024.0;
            let ratio = if zswap_kb > 0 {
                (zswapped_kb as f64) / (zswap_kb as f64)
            } else {
                0.0
            };

            for (id, name, kind, unit, val) in [
                (
                    "zswap.system.size.compressed",
                    "Zswap Pool Size",
                    SensorKind::Memory,
                    "MiB",
                    compressed_mib,
                ),
                (
                    "zswap.system.size.uncompressed",
                    "Zswap Stored (Uncompressed)",
                    SensorKind::Memory,
                    "MiB",
                    orig_mib,
                ),
                (
                    "zswap.system.ratio.compression",
                    "Zswap Compression Ratio",
                    SensorKind::Ratio,
                    "x",
                    ratio,
                ),
            ] {
                out.descriptors.push(SensorDescriptor {
                    id: id.into(),
                    group: "Memory".into(),
                    name: name.into(),
                    kind,
                    unit: unit.into(),
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

        // Optional: also check kernel debug zswap pool_total_size if available
        let _ = read_u64("/sys/kernel/debug/zswap/stored_pages");

        out.summary_patch = Some(SummaryPatch::MemoryRuntime {
            usable_bytes: total_kb.saturating_mul(1024),
            swap_total_bytes: swap_total_kb.saturating_mul(1024),
            zswap_enabled,
            zswap_compressor,
        });

        Ok(out)
    }
}
