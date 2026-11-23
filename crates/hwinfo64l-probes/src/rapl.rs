use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use hwinfo64l_core::probe::SummaryPatch;
use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::{read_trimmed, read_u64, sanitize_key};

#[derive(Debug, Clone, Copy)]
struct EnergyReading {
    energy_uj: u64,
    at: Instant,
}

/// Native Intel/AMD RAPL power telemetry probe (`/sys/class/powercap/intel-rapl*`).
///
/// Computes live Watts from `energy_uj` counter deltas (handles 32-bit wrap via
/// `max_energy_range_uj`) and also reads PL1/PL2 power limits.
pub struct RaplProbe {
    prev_energy: BTreeMap<String, EnergyReading>,
}

impl RaplProbe {
    pub fn new() -> Self {
        Self {
            prev_energy: BTreeMap::new(),
        }
    }
}

impl Default for RaplProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for RaplProbe {
    fn id(&self) -> &str {
        "rapl"
    }

    fn display_name(&self) -> &str {
        "CPU / Platform RAPL Power"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let base = Path::new("/sys/class/powercap");
        let Ok(entries) = fs::read_dir(base) else {
            return Err(ProbeError::Unsupported("/sys/class/powercap missing".into()));
        };

        let mut dirs: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("intel-rapl:"))
                    .unwrap_or(false)
            })
            .collect();
        dirs.sort();

        if dirs.is_empty() {
            return Err(ProbeError::Unsupported("no RAPL domains present".into()));
        }

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();
        let now = Instant::now();
        let mut saw_energy_permission_denied = false;
        let mut pl1_watts = None;
        let mut pl2_watts = None;

        for dir in dirs {
            let domain_id = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "rapl".into());
            let domain_name = read_trimmed(dir.join("name")).unwrap_or_else(|| domain_id.clone());
            let key = sanitize_key(&domain_name);

            // Read PL1 / PL2 limits (world-readable even when energy_uj isn't!)
            if domain_name == "package-0" {
                if let Some(uw) = read_u64(dir.join("constraint_0_power_limit_uw")) {
                    if uw > 0 {
                        let w = (uw as f64) / 1_000_000.0;
                        pl1_watts = Some(w);
                        let id = "rapl.package_0.power.pl1_limit".to_string();
                        out.descriptors.push(SensorDescriptor {
                            id: id.clone(),
                            group: "CPU Power (RAPL)".into(),
                            name: "PL1 Sustained Power Limit".into(),
                            kind: SensorKind::Power,
                            unit: "W".into(),
                            min_expected: Some(0.0),
                            max_expected: None,
                            warn_threshold: None,
                            crit_threshold: None,
                            poll_tier: PollTier::Fast,
                            source: self.id().into(),
                        });
                        out.samples.push(RawSample::new(id, w, ts));
                    }
                }
                if let Some(uw) = read_u64(dir.join("constraint_1_power_limit_uw")) {
                    if uw > 0 {
                        let w = (uw as f64) / 1_000_000.0;
                        pl2_watts = Some(w);
                        let id = "rapl.package_0.power.pl2_limit".to_string();
                        out.descriptors.push(SensorDescriptor {
                            id: id.clone(),
                            group: "CPU Power (RAPL)".into(),
                            name: "PL2 Short-Term Power Limit".into(),
                            kind: SensorKind::Power,
                            unit: "W".into(),
                            min_expected: Some(0.0),
                            max_expected: None,
                            warn_threshold: None,
                            crit_threshold: None,
                            poll_tier: PollTier::Fast,
                            source: self.id().into(),
                        });
                        out.samples.push(RawSample::new(id, w, ts));
                    }
                }
            }

            // Read live energy counter (typically requires CAP_DAC_READ_SEARCH / root)
            let energy_path = dir.join("energy_uj");
            match fs::read_to_string(&energy_path) {
                Ok(raw) => {
                    if let Ok(energy_uj) = raw.trim().parse::<u64>() {
                        let max_range = read_u64(dir.join("max_energy_range_uj")).unwrap_or(0);
                        if let Some(prev) = self.prev_energy.get(&domain_id) {
                            let dt_s = now.duration_since(prev.at).as_secs_f64();
                            if dt_s > 0.05 {
                                let delta_uj = if energy_uj >= prev.energy_uj {
                                    energy_uj - prev.energy_uj
                                } else if max_range > prev.energy_uj {
                                    (max_range - prev.energy_uj).saturating_add(energy_uj)
                                } else {
                                    0
                                };
                                let watts = (delta_uj as f64) / 1_000_000.0 / dt_s;
                                let id = format!("rapl.{key}.power.current");
                                let label = match domain_name.as_str() {
                                    "package-0" => "CPU Package Power".to_string(),
                                    "core" => "CPU Cores Power".to_string(),
                                    "uncore" => "CPU Uncore / iGPU Power".to_string(),
                                    "dram" => "DRAM Power".to_string(),
                                    "psys" => "Platform (PSys) Power".to_string(),
                                    other => format!("RAPL {other} Power"),
                                };
                                out.descriptors.push(SensorDescriptor {
                                    id: id.clone(),
                                    group: "CPU Power (RAPL)".into(),
                                    name: label,
                                    kind: SensorKind::Power,
                                    unit: "W".into(),
                                    min_expected: Some(0.0),
                                    max_expected: pl2_watts.or(pl1_watts),
                                    warn_threshold: None,
                                    crit_threshold: None,
                                    poll_tier: PollTier::Fast,
                                    source: self.id().into(),
                                });
                                out.samples.push(RawSample::new(id, watts, ts));
                            }
                        }
                        self.prev_energy.insert(
                            domain_id,
                            EnergyReading {
                                energy_uj,
                                at: now,
                            },
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    saw_energy_permission_denied = true;
                }
                Err(_) => {}
            }
        }

        if pl1_watts.is_some() || pl2_watts.is_some() {
            out.summary_patch = Some(SummaryPatch::CpuPowerLimits {
                pl1_watts,
                pl2_watts,
            });
        }

        if out.descriptors.is_empty() && saw_energy_permission_denied {
            return Err(ProbeError::RequiresPrivilege(
                "RAPL energy_uj requires hwinfo64ld daemon (CAP_DAC_READ_SEARCH)".into(),
            ));
        }

        Ok(out)
    }
}
