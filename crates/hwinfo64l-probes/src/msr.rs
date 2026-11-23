use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::Path;

use hwinfo64l_core::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
use hwinfo64l_protocol::{PollTier, SensorDescriptor, SensorKind, SensorStatus};

use crate::util::is_effective_root;

const MSR_PERF_STATUS: u64 = 0x198;
const IA32_THERM_STATUS: u64 = 0x19C;
const IA32_PKG_THERM_STATUS: u64 = 0x1B1;
const IA32_PERF_LIMIT_REASONS: u64 = 0x64F;

/// Direct `/dev/cpu/*/msr` Intel CPU voltage (VID), ratio, and throttling /
/// Performance Limit Reasons probe — zero subprocess overhead.
pub struct IntelMsrProbe {
    cpu_indices: Vec<u32>,
    is_intel: bool,
}

impl IntelMsrProbe {
    pub fn new() -> Self {
        let is_intel = fs::read_to_string("/proc/cpuinfo")
            .map(|c| c.contains("GenuineIntel"))
            .unwrap_or(false);
        let mut cpu_indices = Vec::new();
        if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(rest) = name.strip_prefix("cpu") {
                    if let Ok(idx) = rest.parse::<u32>() {
                        cpu_indices.push(idx);
                    }
                }
            }
        }
        cpu_indices.sort();
        Self {
            cpu_indices,
            is_intel,
        }
    }
}

impl Default for IntelMsrProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for IntelMsrProbe {
    fn id(&self) -> &str {
        "intel_msr"
    }

    fn display_name(&self) -> &str {
        "Intel CPU VID & Perf Limit Reasons (MSR)"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Fast
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        if !self.is_intel {
            return Err(ProbeError::Unsupported("non-Intel CPU".into()));
        }
        if !is_effective_root() {
            return Err(ProbeError::RequiresPrivilege(
                "MSR telemetry requires hwinfo64ld daemon (CAP_SYS_RAWIO)".into(),
            ));
        }
        if !Path::new("/dev/cpu/0/msr").exists() {
            return Err(ProbeError::Unsupported(
                "/dev/cpu/0/msr unavailable (load `msr` kernel module)".into(),
            ));
        }

        let mut out = ProbeOutput::default();
        let ts = now_epoch_ms();
        let mut vids = Vec::new();

        for &cpu in &self.cpu_indices {
            let path = format!("/dev/cpu/{cpu}/msr");
            let Ok(file) = File::open(&path) else {
                continue;
            };

            if let Some(perf) = read_msr(&file, MSR_PERF_STATUS) {
                // Intel Core / Atom (including Jasper Lake): bits 47:32 = VID * 2^-13 V
                let raw_vid = (perf >> 32) & 0xFFFF;
                let volts = (raw_vid as f64) / 8192.0;
                if (0.2..=2.2).contains(&volts) {
                    vids.push(volts);
                    let id = format!("msr.cpu{cpu}.volt.vid");
                    out.descriptors.push(SensorDescriptor {
                        id: id.clone(),
                        group: "CPU Voltages (MSR)".into(),
                        name: format!("Core #{cpu} VID"),
                        kind: SensorKind::Voltage,
                        unit: "V".into(),
                        min_expected: Some(0.4),
                        max_expected: Some(1.6),
                        warn_threshold: None,
                        crit_threshold: None,
                        poll_tier: PollTier::Fast,
                        source: self.id().into(),
                    });
                    out.samples.push(RawSample::new(id, volts, ts));
                }
            }
        }

        if !vids.is_empty() {
            let avg_vid: f64 = vids.iter().sum::<f64>() / (vids.len() as f64);
            let id = "msr.pkg0.volt.vcore_avg".to_string();
            out.descriptors.push(SensorDescriptor {
                id: id.clone(),
                group: "CPU Voltages (MSR)".into(),
                name: "CPU Core Voltage (Avg VID)".into(),
                kind: SensorKind::Voltage,
                unit: "V".into(),
                min_expected: Some(0.4),
                max_expected: Some(1.6),
                warn_threshold: None,
                crit_threshold: None,
                poll_tier: PollTier::Fast,
                source: self.id().into(),
            });
            out.samples.push(RawSample::new(id, avg_vid, ts));
        }

        // Package-wide thermal & performance limit reasons on CPU 0
        if let Ok(file) = File::open("/dev/cpu/0/msr") {
            if let Some(therm) = read_msr(&file, IA32_THERM_STATUS) {
                push_flag(
                    &mut out,
                    self.id(),
                    "msr.cpu0.flag.thermal_throttle",
                    "Core Thermal Throttling",
                    (therm & (1 << 0)) != 0,
                    ts,
                );
                push_flag(
                    &mut out,
                    self.id(),
                    "msr.cpu0.flag.prochot",
                    "Core PROCHOT# Active",
                    (therm & (1 << 2)) != 0,
                    ts,
                );
                push_flag(
                    &mut out,
                    self.id(),
                    "msr.cpu0.flag.power_limit_notif",
                    "Core Power Limit Notification",
                    (therm & (1 << 10)) != 0,
                    ts,
                );
            }

            if let Some(pkg) = read_msr(&file, IA32_PKG_THERM_STATUS) {
                push_flag(
                    &mut out,
                    self.id(),
                    "msr.pkg0.flag.pkg_thermal_throttle",
                    "Package Thermal Throttling",
                    (pkg & (1 << 0)) != 0,
                    ts,
                );
                push_flag(
                    &mut out,
                    self.id(),
                    "msr.pkg0.flag.pkg_power_limit",
                    "Package Power Limit Notification",
                    (pkg & (1 << 10)) != 0,
                    ts,
                );
            }

            if let Some(plr) = read_msr(&file, IA32_PERF_LIMIT_REASONS) {
                let flags = [
                    ("prochot", "IA Limit: PROCHOT", 0u32),
                    ("thermal", "IA Limit: Thermal", 1),
                    ("vr_thermal", "IA Limit: VR Thermal Alert", 6),
                    ("edp_current", "IA Limit: Electrical Design Point (EDP)", 8),
                    ("max_turbo", "IA Limit: Max Turbo Limit", 10),
                    ("pl1_power", "IA Limit: Package Power Limit (PL1)", 11),
                    ("pl2_power", "IA Limit: Package Power Limit (PL2)", 12),
                ];
                for (key, label, bit) in flags {
                    let active = (plr & (1u64 << bit)) != 0;
                    push_flag(
                        &mut out,
                        self.id(),
                        &format!("msr.pkg0.limit.{key}"),
                        label,
                        active,
                        ts,
                    );
                }
            }
        }

        if out.descriptors.is_empty() {
            return Err(ProbeError::Unsupported("no readable Intel MSR counters".into()));
        }

        Ok(out)
    }
}

fn read_msr(file: &File, reg: u64) -> Option<u64> {
    let mut buf = [0u8; 8];
    file.read_exact_at(&mut buf, reg).ok()?;
    Some(u64::from_le_bytes(buf))
}

fn push_flag(
    out: &mut ProbeOutput,
    source: &str,
    id: &str,
    name: &str,
    active: bool,
    ts: u64,
) {
    out.descriptors.push(SensorDescriptor {
        id: id.to_string(),
        group: "CPU Performance Limit Reasons".into(),
        name: name.to_string(),
        kind: SensorKind::BooleanFlag,
        unit: String::new(),
        min_expected: Some(0.0),
        max_expected: Some(1.0),
        warn_threshold: None,
        crit_threshold: None,
        poll_tier: PollTier::Fast,
        source: source.to_string(),
    });
    let mut sample = RawSample::new(id, if active { 1.0 } else { 0.0 }, ts);
    sample.status_override = Some(if active {
        SensorStatus::Warning
    } else {
        SensorStatus::Ok
    });
    out.samples.push(sample);
}
