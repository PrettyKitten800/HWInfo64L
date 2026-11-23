use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use hwinfo64l_core::probe::SummaryPatch;
use hwinfo64l_core::{Probe, ProbeError, ProbeOutput};
use hwinfo64l_protocol::{
    CpuSummary, DriveSummary, GpuSummary, InventoryKind, InventoryNode, MotherboardSummary,
    OsSummary, PollTier, PropertyValue,
};

use crate::util::{read_trimmed, read_u64};

/// Static probe populating SystemSummary & Hardware Tree inventory nodes
/// for OS, CPU, Motherboard/BIOS, GPUs, and Block Drives.
pub struct SystemInventoryProbe;

impl SystemInventoryProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemInventoryProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for SystemInventoryProbe {
    fn id(&self) -> &str {
        "system_inventory"
    }

    fn display_name(&self) -> &str {
        "System & Hardware Inventory"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Static
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let mut out = ProbeOutput::default();

        let hostname = read_trimmed("/etc/hostname")
            .or_else(|| read_trimmed("/proc/sys/kernel/hostname"))
            .unwrap_or_else(|| "unknown".to_string());

        let os = read_os_summary();
        let cpu = read_cpu_summary();
        let motherboard = read_motherboard_summary();
        let gpus = read_gpu_summaries();
        let drives = read_drive_summaries();

        // Build hardware tree inventory nodes.
        let mut sys_props = BTreeMap::new();
        sys_props.insert("Hostname".into(), PropertyValue::Text(hostname.clone()));
        sys_props.insert("OS".into(), PropertyValue::Text(os.pretty_name.clone()));
        sys_props.insert("Kernel".into(), PropertyValue::Text(os.kernel.clone()));
        sys_props.insert("Architecture".into(), PropertyValue::Text(os.arch.clone()));
        out.inventory.push(InventoryNode {
            id: "system".into(),
            parent_id: None,
            kind: InventoryKind::System,
            label: hostname.clone(),
            properties: sys_props,
            source: self.id().into(),
        });

        let mut cpu_props = BTreeMap::new();
        cpu_props.insert("Model".into(), PropertyValue::Text(cpu.model_name.clone()));
        cpu_props.insert("Vendor".into(), PropertyValue::Text(cpu.vendor.clone()));
        cpu_props.insert(
            "Physical Cores".into(),
            PropertyValue::Integer(i64::from(cpu.physical_cores)),
        );
        cpu_props.insert(
            "Logical CPUs".into(),
            PropertyValue::Integer(i64::from(cpu.logical_cpus)),
        );
        if let Some(min) = cpu.min_mhz {
            cpu_props.insert("Min Clock (MHz)".into(), PropertyValue::Float(min));
        }
        if let Some(max) = cpu.max_mhz {
            cpu_props.insert("Max Clock (MHz)".into(), PropertyValue::Float(max));
        }
        if let Some(ref c) = cpu.l1_cache {
            cpu_props.insert("L1 Cache".into(), PropertyValue::Text(c.clone()));
        }
        if let Some(ref c) = cpu.l2_cache {
            cpu_props.insert("L2 Cache".into(), PropertyValue::Text(c.clone()));
        }
        if let Some(ref c) = cpu.l3_cache {
            cpu_props.insert("L3 Cache".into(), PropertyValue::Text(c.clone()));
        }
        out.inventory.push(InventoryNode {
            id: "cpu0".into(),
            parent_id: Some("system".into()),
            kind: InventoryKind::Cpu,
            label: cpu.model_name.clone(),
            properties: cpu_props,
            source: self.id().into(),
        });

        let mut mb_props = BTreeMap::new();
        mb_props.insert(
            "System Vendor".into(),
            PropertyValue::Text(motherboard.sys_vendor.clone()),
        );
        mb_props.insert(
            "Product Name".into(),
            PropertyValue::Text(motherboard.product_name.clone()),
        );
        mb_props.insert(
            "Board Vendor".into(),
            PropertyValue::Text(motherboard.board_vendor.clone()),
        );
        mb_props.insert(
            "Board Name".into(),
            PropertyValue::Text(motherboard.board_name.clone()),
        );
        mb_props.insert(
            "BIOS Vendor".into(),
            PropertyValue::Text(motherboard.bios_vendor.clone()),
        );
        mb_props.insert(
            "BIOS Version".into(),
            PropertyValue::Text(motherboard.bios_version.clone()),
        );
        mb_props.insert(
            "BIOS Date".into(),
            PropertyValue::Text(motherboard.bios_date.clone()),
        );
        out.inventory.push(InventoryNode {
            id: "motherboard".into(),
            parent_id: Some("system".into()),
            kind: InventoryKind::Motherboard,
            label: format!("{} {}", motherboard.board_vendor, motherboard.board_name)
                .trim()
                .to_string(),
            properties: mb_props,
            source: self.id().into(),
        });

        for gpu in &gpus {
            let mut props = BTreeMap::new();
            props.insert("Name".into(), PropertyValue::Text(gpu.name.clone()));
            props.insert("Vendor".into(), PropertyValue::Text(gpu.vendor.clone()));
            props.insert("Driver".into(), PropertyValue::Text(gpu.driver.clone()));
            if let Some(ref slot) = gpu.pci_slot {
                props.insert("PCI Slot".into(), PropertyValue::Text(slot.clone()));
            }
            out.inventory.push(InventoryNode {
                id: format!("gpu_{}", gpu.id),
                parent_id: Some("system".into()),
                kind: InventoryKind::Gpu,
                label: gpu.name.clone(),
                properties: props,
                source: self.id().into(),
            });
        }

        for drive in &drives {
            let mut props = BTreeMap::new();
            props.insert("Device".into(), PropertyValue::Text(drive.device.clone()));
            props.insert("Model".into(), PropertyValue::Text(drive.model.clone()));
            props.insert(
                "Transport".into(),
                PropertyValue::Text(drive.transport.clone()),
            );
            props.insert(
                "Size (GB)".into(),
                PropertyValue::Float((drive.size_bytes as f64) / 1e9),
            );
            props.insert(
                "Media".into(),
                PropertyValue::Text(if drive.rotational { "HDD" } else { "SSD" }.into()),
            );
            out.inventory.push(InventoryNode {
                id: format!("drive_{}", drive.device),
                parent_id: Some("system".into()),
                kind: InventoryKind::Storage,
                label: format!("{} ({})", drive.model, drive.device),
                properties: props,
                source: self.id().into(),
            });
        }

        // Apply full summary (memory probe will enrich memory fields).
        let summary = hwinfo64l_protocol::SystemSummary {
            hostname,
            os,
            cpu,
            motherboard,
            memory: Default::default(),
            gpus,
            drives,
        };
        out.summary_patch = Some(SummaryPatch::Full(Box::new(summary)));
        Ok(out)
    }
}

fn read_os_summary() -> OsSummary {
    let mut pretty_name = "Linux".to_string();
    if let Ok(content) = fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
                pretty_name = rest.trim_matches('"').to_string();
                break;
            }
        }
    }

    let kernel = read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into());
    let arch = std::env::consts::ARCH.to_string();
    let uptime_secs = read_trimmed("/proc/uptime")
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0);

    OsSummary {
        pretty_name,
        kernel,
        arch,
        uptime_secs,
    }
}

fn read_cpu_summary() -> CpuSummary {
    let mut model_name = String::new();
    let mut vendor = String::new();
    let mut logical_cpus = 0u32;
    let mut core_ids: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut cur_phys = 0u32;

    if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim();
            match key {
                "processor" => logical_cpus = logical_cpus.saturating_add(1),
                "model name" if model_name.is_empty() => model_name = val.to_string(),
                "vendor_id" if vendor.is_empty() => vendor = val.to_string(),
                "physical id" => cur_phys = val.parse().unwrap_or(0),
                "core id" => {
                    let cur_core = val.parse().unwrap_or(0);
                    core_ids.insert((cur_phys, cur_core));
                }
                _ => {}
            }
        }
    }

    let physical_cores = if core_ids.is_empty() {
        logical_cpus.max(1)
    } else {
        u32::try_from(core_ids.len()).unwrap_or(logical_cpus)
    };

    let min_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq")
        .map(|khz| (khz as f64) / 1000.0);
    let max_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .map(|khz| (khz as f64) / 1000.0);

    let (l1_cache, l2_cache, l3_cache) = read_cpu_caches();

    CpuSummary {
        model_name: if model_name.is_empty() {
            "Unknown CPU".into()
        } else {
            model_name
        },
        vendor,
        physical_cores,
        logical_cpus: logical_cpus.max(1),
        min_mhz,
        max_mhz,
        pl1_watts: None,
        pl2_watts: None,
        l1_cache,
        l2_cache,
        l3_cache,
    }
}

fn read_cpu_caches() -> (Option<String>, Option<String>, Option<String>) {
    let base = Path::new("/sys/devices/system/cpu/cpu0/cache");
    let mut l1_parts = Vec::new();
    let mut l2 = None;
    let mut l3 = None;

    let Ok(entries) = fs::read_dir(base) else {
        return (None, None, None);
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Some(level) = read_trimmed(p.join("level")) else {
            continue;
        };
        let Some(size) = read_trimmed(p.join("size")) else {
            continue;
        };
        let ctype = read_trimmed(p.join("type")).unwrap_or_default();
        match level.as_str() {
            "1" => l1_parts.push(format!("{size} {ctype}")),
            "2" => l2 = Some(size),
            "3" => l3 = Some(size),
            _ => {}
        }
    }
    l1_parts.sort();
    let l1 = if l1_parts.is_empty() {
        None
    } else {
        Some(l1_parts.join(" + "))
    };
    (l1, l2, l3)
}

fn read_motherboard_summary() -> MotherboardSummary {
    let dmi = Path::new("/sys/devices/virtual/dmi/id");
    MotherboardSummary {
        sys_vendor: read_trimmed(dmi.join("sys_vendor")).unwrap_or_default(),
        product_name: read_trimmed(dmi.join("product_name")).unwrap_or_default(),
        board_vendor: read_trimmed(dmi.join("board_vendor")).unwrap_or_default(),
        board_name: read_trimmed(dmi.join("board_name")).unwrap_or_default(),
        bios_vendor: read_trimmed(dmi.join("bios_vendor")).unwrap_or_default(),
        bios_version: read_trimmed(dmi.join("bios_version")).unwrap_or_default(),
        bios_date: read_trimmed(dmi.join("bios_date")).unwrap_or_default(),
    }
}

fn read_gpu_summaries() -> Vec<GpuSummary> {
    let mut gpus = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return gpus;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        if !dev.exists() {
            continue;
        }
        let vendor_id = read_trimmed(dev.join("vendor")).unwrap_or_default();
        let vendor = match vendor_id.as_str() {
            "0x8086" => "Intel".to_string(),
            "0x1002" => "AMD".to_string(),
            "0x10de" => "NVIDIA".to_string(),
            other => other.to_string(),
        };
        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "unknown".into());
        let pci_slot = fs::read_link(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()));

        let pci_name = pci_slot
            .as_deref()
            .and_then(lookup_lspci_name)
            .unwrap_or_else(|| format!("{vendor} GPU ({driver})"));

        gpus.push(GpuSummary {
            id: name,
            name: pci_name,
            vendor,
            driver,
            pci_slot,
        });
    }
    gpus.sort_by(|a, b| a.id.cmp(&b.id));
    gpus
}

fn lookup_lspci_name(slot: &str) -> Option<String> {
    let out = Command::new("lspci").args(["-s", slot]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let first = line.lines().next()?;
    let (_, after_colon) = first.split_once(": ")?;
    Some(after_colon.trim().to_string())
}

fn read_drive_summaries() -> Vec<DriveSummary> {
    let mut drives = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return drives;
    };
    for entry in entries.flatten() {
        let dev = entry.file_name().to_string_lossy().to_string();
        if dev.starts_with("loop")
            || dev.starts_with("ram")
            || dev.starts_with("dm-")
            || dev.starts_with("zram")
            || dev.starts_with("sr")
        {
            continue;
        }
        let path = entry.path();
        let sectors = read_u64(path.join("size")).unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        let size_bytes = sectors.saturating_mul(512);
        let rotational = read_u64(path.join("queue/rotational")).unwrap_or(0) == 1;
        let model = read_trimmed(path.join("device/model")).unwrap_or_else(|| dev.clone());
        let transport = if dev.starts_with("nvme") {
            "NVMe".to_string()
        } else if dev.starts_with("mmcblk") {
            "eMMC/SD".to_string()
        } else {
            "SATA/SCSI".to_string()
        };

        drives.push(DriveSummary {
            device: dev,
            model,
            transport,
            size_bytes,
            rotational,
            smart_health: None,
        });
    }
    drives.sort_by(|a, b| a.device.cmp(&b.device));
    drives
}
