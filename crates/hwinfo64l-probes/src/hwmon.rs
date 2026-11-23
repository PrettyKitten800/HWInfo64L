use std::fs;
use std::path::Path;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind};

use crate::util::{read_i64, read_trimmed, sanitize_key};

/// Native Linux `/sys/class/hwmon` probe covering temperatures, fans, voltages,
/// power, and current across CPUs, GPUs, motherboards, and NVMe drives.
pub struct HwmonProbe;

impl HwmonProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HwmonProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for HwmonProbe {
    fn id(&self) -> &str {
        "hwmon"
    }

    fn display_name(&self) -> &str {
        "Linux hwmon Sensors"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();

        let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
            return Err(ProbeError::Unsupported("/sys/class/hwmon missing".into()));
        };

        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();

        for dir in dirs {
            let chip_name = read_trimmed(dir.join("name")).unwrap_or_else(|| "hwmon".into());
            let dir_key = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "hwmon0".into());
            let chip_key = sanitize_key(&chip_name);
            let group = format_hwmon_group(&chip_name, &dir_key);

            let Ok(files) = fs::read_dir(&dir) else {
                continue;
            };
            let mut file_names: Vec<String> = files
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            file_names.sort();

            for fname in file_names {
                if let Some(prefix) = fname.strip_suffix("_input") {
                    read_hwmon_channel(
                        &dir, &chip_key, &dir_key, &group, prefix, ts, &mut out,
                    );
                } else if let Some(prefix) = fname.strip_suffix("_average") {
                    if !dir.join(format!("{prefix}_input")).exists() {
                        read_hwmon_channel_custom(
                            &dir, &chip_key, &dir_key, &group, prefix, &fname, ts, &mut out,
                        );
                    }
                }
            }
        }

        if out.descriptors.is_empty() {
            return Err(ProbeError::Unsupported("no hwmon sensors found".into()));
        }
        Ok(out)
    }
}

fn format_hwmon_group(chip_name: &str, dir_key: &str) -> String {
    match chip_name {
        "coretemp" => "CPU Temperatures (coretemp)".into(),
        "k10temp" | "zenpower" => format!("CPU Temperatures ({chip_name})"),
        "acpitz" => "ACPI Thermal Zones".into(),
        "amdgpu" => "GPU Sensors (amdgpu)".into(),
        "i915" | "xe" => format!("GPU Sensors ({chip_name})"),
        "nvme" => format!("NVMe Drive ({dir_key})"),
        _ => format!("Hardware Sensors ({chip_name})"),
    }
}

fn read_hwmon_channel(
    dir: &Path,
    chip_key: &str,
    dir_key: &str,
    group: &str,
    prefix: &str,
    ts: u64,
    out: &mut ProbeOutput,
) {
    let fname = format!("{prefix}_input");
    read_hwmon_channel_custom(dir, chip_key, dir_key, group, prefix, &fname, ts, out);
}

#[allow(clippy::too_many_arguments)]
fn read_hwmon_channel_custom(
    dir: &Path,
    chip_key: &str,
    dir_key: &str,
    group: &str,
    prefix: &str,
    input_file: &str,
    ts: u64,
    out: &mut ProbeOutput,
) {
    let Some(raw) = read_i64(dir.join(input_file)) else {
        return;
    };

    let (category, kind, unit, scale) = if prefix.starts_with("temp") {
        ("temp", SensorKind::Temperature, "°C", 1000.0)
    } else if prefix.starts_with("fan") {
        ("fan", SensorKind::FanRpm, "RPM", 1.0)
    } else if prefix.starts_with("in") {
        ("volt", SensorKind::Voltage, "V", 1000.0)
    } else if prefix.starts_with("power") {
        ("power", SensorKind::Power, "W", 1_000_000.0)
    } else if prefix.starts_with("curr") {
        ("curr", SensorKind::Current, "A", 1000.0)
    } else {
        return;
    };

    let label = read_trimmed(dir.join(format!("{prefix}_label")))
        .unwrap_or_else(|| prefix.to_string());
    let metric_key = sanitize_key(&label);
    let sensor_id = format!("hwmon.{chip_key}_{dir_key}.{category}.{metric_key}");

    let value = (raw as f64) / scale;
    let warn_threshold = if kind == SensorKind::Temperature {
        read_i64(dir.join(format!("{prefix}_max"))).map(|v| (v as f64) / scale)
    } else {
        None
    };
    let crit_threshold = if kind == SensorKind::Temperature {
        read_i64(dir.join(format!("{prefix}_crit"))).map(|v| (v as f64) / scale)
    } else {
        None
    };

    out.descriptors.push(SensorDescriptor {
        id: sensor_id.clone(),
        group: group.to_string(),
        name: label,
        kind,
        unit: unit.to_string(),
        min_expected: Some(0.0),
        max_expected: crit_threshold.or(warn_threshold),
        warn_threshold,
        crit_threshold,
        poll_tier: PollTier::Fast,
        source: "hwmon".into(),
    });
    out.samples.push(RawSample::new(sensor_id, value, ts));
}
