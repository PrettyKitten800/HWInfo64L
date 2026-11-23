use std::fs;
use std::time::Duration;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::{read_trimmed, read_u64, sanitize_key};

/// Native `/sys/class/power_supply` battery & AC adapter probe for laptops/UPSes.
/// Marks itself `Unsupported` cleanly on desktop/mini-PC systems without batteries.
pub struct BatteryProbe;

impl BatteryProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BatteryProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for BatteryProbe {
    fn id(&self) -> &str {
        "battery"
    }

    fn display_name(&self) -> &str {
        "Battery & Power Supply"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Slow
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
            return Err(ProbeError::Unsupported("/sys/class/power_supply missing".into()));
        };

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        let mut dirs: Vec<_> = entries.flatten().collect();
        dirs.sort_by_key(|e| e.file_name());

        for entry in dirs {
            let dev = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let ptype = read_trimmed(path.join("type")).unwrap_or_default();
            if ptype != "Battery" {
                continue;
            }
            let key = sanitize_key(&dev);
            let group = format!("Battery ({dev})");

            if let Some(cap) = read_u64(path.join("capacity")) {
                push_bat(
                    &mut out,
                    self.id(),
                    &format!("battery.{key}.util.charge"),
                    &group,
                    "Charge Level",
                    SensorKind::Utilization,
                    "%",
                    cap as f64,
                    Some(100.0),
                    ts,
                );
            }
            if let Some(uv) = read_u64(path.join("voltage_now")) {
                push_bat(
                    &mut out,
                    self.id(),
                    &format!("battery.{key}.volt.now"),
                    &group,
                    "Battery Voltage",
                    SensorKind::Voltage,
                    "V",
                    (uv as f64) / 1_000_000.0,
                    None,
                    ts,
                );
            }
            if let Some(uw) = read_u64(path.join("power_now")) {
                push_bat(
                    &mut out,
                    self.id(),
                    &format!("battery.{key}.power.rate"),
                    &group,
                    "Charge / Discharge Rate",
                    SensorKind::Power,
                    "W",
                    (uw as f64) / 1_000_000.0,
                    None,
                    ts,
                );
            }
            if let (Some(full), Some(design)) = (
                read_u64(path.join("energy_full")).or_else(|| read_u64(path.join("charge_full"))),
                read_u64(path.join("energy_full_design"))
                    .or_else(|| read_u64(path.join("charge_full_design"))),
            ) {
                if design > 0 {
                    let wear = ((1.0 - (full as f64 / design as f64)) * 100.0).clamp(0.0, 100.0);
                    push_bat(
                        &mut out,
                        self.id(),
                        &format!("battery.{key}.util.wear"),
                        &group,
                        "Battery Wear Level",
                        SensorKind::Utilization,
                        "%",
                        wear,
                        Some(100.0),
                        ts,
                    );
                }
            }
            if let Some(cycles) = read_u64(path.join("cycle_count")) {
                if cycles > 0 {
                    push_bat(
                        &mut out,
                        self.id(),
                        &format!("battery.{key}.counter.cycles"),
                        &group,
                        "Cycle Count",
                        SensorKind::Counter,
                        "",
                        cycles as f64,
                        None,
                        ts,
                    );
                }
            }
        }

        if out.descriptors.is_empty() {
            return Err(ProbeError::Unsupported("no system battery present".into()));
        }
        Ok(out)
    }
}

#[allow(clippy::too_many_arguments)]
fn push_bat(
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
        poll_tier: PollTier::Slow,
        source: source.to_string(),
    });
    out.samples.push(RawSample::new(id, value, ts));
}
