use std::process::Command;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::resolve_trusted_binary;

/// NVIDIA GPU telemetry probe via `nvidia-smi` CSV query.
/// Cleanly marks itself `Unsupported` on non-NVIDIA systems so it incurs zero overhead.
pub struct NvidiaGpuProbe;

impl NvidiaGpuProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NvidiaGpuProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for NvidiaGpuProbe {
    fn id(&self) -> &str {
        "nvidia_gpu"
    }

    fn display_name(&self) -> &str {
        "NVIDIA GPU Telemetry (NVML)"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let Some(bin) = resolve_trusted_binary("nvidia-smi") else {
            return Err(ProbeError::Unsupported("nvidia-smi not present".into()));
        };
        let output = match Command::new(bin)
            .args([
                "--query-gpu=index,name,temperature.gpu,utilization.gpu,utilization.memory,memory.used,memory.total,clocks.gr,clocks.mem,power.draw,power.limit,fan.speed",
                "--format=csv,noheader,nounits",
            ])
            .output()
        {
            Ok(o) => o,
            Err(e) => return Err(ProbeError::Io(e)),
        };

        if !output.status.success() {
            return Err(ProbeError::Unsupported("no NVIDIA GPU active".into()));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        for line in text.lines() {
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            if cols.len() < 12 {
                continue;
            }
            let idx = cols[0];
            let name = cols[1];
            let group = format!("GPU #{idx}: {name}");
            let vram_total = cols[6].parse::<f64>().ok();
            let power_limit = cols[10].parse::<f64>().ok();

            struct NvidiaMetric<'a> {
                metric: &'a str,
                label: &'a str,
                kind: SensorKind,
                unit: &'a str,
                raw: &'a str,
                max_exp: Option<f64>,
                warn: Option<f64>,
                crit: Option<f64>,
            }

            let metrics = [
                NvidiaMetric { metric: "temp.core", label: "GPU Temperature", kind: SensorKind::Temperature, unit: "°C", raw: cols[2], max_exp: Some(95.0), warn: Some(83.0), crit: Some(92.0) },
                NvidiaMetric { metric: "util.core", label: "GPU Core Load", kind: SensorKind::Utilization, unit: "%", raw: cols[3], max_exp: Some(100.0), warn: None, crit: None },
                NvidiaMetric { metric: "util.mem", label: "GPU Memory Controller Load", kind: SensorKind::Utilization, unit: "%", raw: cols[4], max_exp: Some(100.0), warn: None, crit: None },
                NvidiaMetric { metric: "memory.vram_used", label: "VRAM Used", kind: SensorKind::Memory, unit: "MiB", raw: cols[5], max_exp: vram_total, warn: None, crit: None },
                NvidiaMetric { metric: "clock.core", label: "GPU Graphics Clock", kind: SensorKind::Clock, unit: "MHz", raw: cols[7], max_exp: None, warn: None, crit: None },
                NvidiaMetric { metric: "clock.mem", label: "GPU Memory Clock", kind: SensorKind::Clock, unit: "MHz", raw: cols[8], max_exp: None, warn: None, crit: None },
                NvidiaMetric { metric: "power.draw", label: "GPU Power", kind: SensorKind::Power, unit: "W", raw: cols[9], max_exp: power_limit, warn: None, crit: None },
                NvidiaMetric { metric: "fan.speed", label: "GPU Fan Speed", kind: SensorKind::Utilization, unit: "%", raw: cols[11], max_exp: Some(100.0), warn: None, crit: None },
            ];

            for m in metrics {
                let Ok(val) = m.raw.parse::<f64>() else {
                    continue;
                };
                let id = format!("nvidia.gpu{idx}.{}", m.metric);
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: m.label.into(),
                    kind: m.kind,
                    unit: m.unit.into(),
                    min_expected: Some(0.0),
                    max_expected: m.max_exp,
                    warn_threshold: m.warn,
                    crit_threshold: m.crit,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, val, ts));
            }
        }

        if out.descriptors.is_empty() {
            return Err(ProbeError::Unsupported("no NVIDIA GPUs reported".into()));
        }
        Ok(out)
    }
}
