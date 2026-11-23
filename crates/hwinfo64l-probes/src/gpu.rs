use std::fs;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::read_u64;

/// Native `/sys/class/drm` GPU probe covering Intel (`i915`/`xe`) and AMD (`amdgpu`)
/// clocks, utilization, and VRAM without subprocess overhead.
pub struct GpuProbe;

impl GpuProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for GpuProbe {
    fn id(&self) -> &str {
        "gpu_drm"
    }

    fn display_name(&self) -> &str {
        "GPU Telemetry (DRM)"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let Ok(entries) = fs::read_dir("/sys/class/drm") else {
            return Err(ProbeError::Unsupported("/sys/class/drm missing".into()));
        };

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        let mut cards: Vec<_> = entries
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("card") && !n.contains('-')
            })
            .collect();
        cards.sort_by_key(|e| e.file_name());

        for entry in cards {
            let card = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let dev = path.join("device");
            let group = format!("GPU ({card})");

            // Intel i915 / xe frequencies
            let intel_cur = read_u64(path.join("gt_act_freq_mhz"))
                .or_else(|| read_u64(path.join("gt_cur_freq_mhz")))
                .or_else(|| read_u64(path.join("gt/gt0/rps_act_freq_mhz")));
            let intel_max = read_u64(path.join("gt_max_freq_mhz"))
                .or_else(|| read_u64(path.join("gt/gt0/rps_max_freq_mhz")));

            if let Some(mhz) = intel_cur {
                let id = format!("gpu.{card}.clock.core");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "GPU Clock".into(),
                    kind: SensorKind::Clock,
                    unit: "MHz".into(),
                    min_expected: Some(0.0),
                    max_expected: intel_max.map(|v| v as f64),
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, mhz as f64, ts));
            }

            // AMD amdgpu busy % and VRAM
            if let Some(busy) = read_u64(dev.join("gpu_busy_percent")) {
                let id = format!("gpu.{card}.util.core");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "GPU Core Load".into(),
                    kind: SensorKind::Utilization,
                    unit: "%".into(),
                    min_expected: Some(0.0),
                    max_expected: Some(100.0),
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(id, busy as f64, ts));
            }

            if let Some(vram_used) = read_u64(dev.join("mem_info_vram_used")) {
                let vram_total = read_u64(dev.join("mem_info_vram_total"));
                let id = format!("gpu.{card}.memory.vram_used");
                out.descriptors.push(SensorDescriptor {
                    id: id.clone(),
                    group: group.clone(),
                    name: "VRAM Used".into(),
                    kind: SensorKind::Memory,
                    unit: "MiB".into(),
                    min_expected: Some(0.0),
                    max_expected: vram_total.map(|b| (b as f64) / (1024.0 * 1024.0)),
                    warn_threshold: None,
                    crit_threshold: None,
                    poll_tier: PollTier::Fast,
                    source: self.id().into(),
                });
                out.samples.push(RawSample::new(
                    id,
                    (vram_used as f64) / (1024.0 * 1024.0),
                    ts,
                ));
            }
        }

        if out.descriptors.is_empty() {
            return Err(ProbeError::Unsupported(
                "no readable DRM GPU counters found".into(),
            ));
        }

        Ok(out)
    }
}
