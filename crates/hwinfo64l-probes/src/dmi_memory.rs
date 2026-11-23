use std::collections::BTreeMap;
use std::process::Command;

use hwinfo64l_core::probe::SummaryPatch;
use hwinfo64l_core::{Probe, ProbeError, ProbeOutput};
use hwinfo64l_protocol::{
    InventoryKind, InventoryNode, MemoryModuleSummary, PollTier, PropertyValue,
};

use crate::util::is_effective_root;

/// Static RAM SMBIOS/DMI probe.
///
/// Uses `dmidecode -t memory` when running privileged (exact voltage, ECC,
/// speeds), and gracefully falls back to unprivileged `udevadm info
/// /sys/class/dmi/id` which exposes full Type 16/17 memory metadata on Linux.
pub struct DmiMemoryProbe;

impl DmiMemoryProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DmiMemoryProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for DmiMemoryProbe {
    fn id(&self) -> &str {
        "dmi_memory"
    }

    fn display_name(&self) -> &str {
        "SMBIOS / DMI Memory Inventory"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Static
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let parsed = if is_effective_root() {
            parse_dmidecode().or_else(parse_udev_dmi)
        } else {
            parse_udev_dmi()
        };

        let Some(mem) = parsed else {
            return Err(ProbeError::Unsupported(
                "SMBIOS memory tables unavailable".into(),
            ));
        };

        let mut out = ProbeOutput::default();

        let mut array_props = BTreeMap::new();
        array_props.insert(
            "Total Physical (GB)".into(),
            PropertyValue::Float((mem.total_physical_bytes as f64) / (1024.0 * 1024.0 * 1024.0)),
        );
        if let Some(max) = mem.max_capacity_bytes {
            array_props.insert(
                "Max Capacity (GB)".into(),
                PropertyValue::Float((max as f64) / (1024.0 * 1024.0 * 1024.0)),
            );
        }
        if let Some(ref mt) = mem.mem_type {
            array_props.insert("Type".into(), PropertyValue::Text(mt.clone()));
        }
        if let Some(spd) = mem.configured_speed_mts {
            array_props.insert(
                "Configured Speed (MT/s)".into(),
                PropertyValue::Integer(i64::from(spd)),
            );
        }
        if let Some(spd) = mem.rated_speed_mts {
            array_props.insert(
                "Rated Speed (MT/s)".into(),
                PropertyValue::Integer(i64::from(spd)),
            );
        }
        array_props.insert(
            "Populated Devices".into(),
            PropertyValue::Integer(i64::from(mem.channels_populated)),
        );

        out.inventory.push(InventoryNode {
            id: "memory_array".into(),
            parent_id: Some("system".into()),
            kind: InventoryKind::MemoryArray,
            label: format!(
                "System Memory ({:.0} GB {})",
                (mem.total_physical_bytes as f64) / (1024.0 * 1024.0 * 1024.0),
                mem.mem_type.as_deref().unwrap_or("RAM")
            ),
            properties: array_props,
            source: self.id().into(),
        });

        for (idx, m) in mem.modules.iter().enumerate() {
            let mut props = BTreeMap::new();
            props.insert("Locator".into(), PropertyValue::Text(m.locator.clone()));
            props.insert("Bank".into(), PropertyValue::Text(m.bank_locator.clone()));
            props.insert(
                "Size (GB)".into(),
                PropertyValue::Float((m.size_bytes as f64) / (1024.0 * 1024.0 * 1024.0)),
            );
            props.insert("Type".into(), PropertyValue::Text(m.mem_type.clone()));
            props.insert(
                "Form Factor".into(),
                PropertyValue::Text(m.form_factor.clone()),
            );
            props.insert(
                "Manufacturer".into(),
                PropertyValue::Text(m.manufacturer.clone()),
            );
            props.insert(
                "Part Number".into(),
                PropertyValue::Text(m.part_number.clone()),
            );
            if let Some(s) = m.speed_mts {
                props.insert("Rated Speed (MT/s)".into(), PropertyValue::Integer(i64::from(s)));
            }
            if let Some(s) = m.configured_speed_mts {
                props.insert(
                    "Configured Speed (MT/s)".into(),
                    PropertyValue::Integer(i64::from(s)),
                );
            }
            if let Some(mv) = m.voltage_mv {
                props.insert(
                    "Voltage (V)".into(),
                    PropertyValue::Float((mv as f64) / 1000.0),
                );
            }
            if let Some(r) = m.rank {
                props.insert("Rank".into(), PropertyValue::Integer(i64::from(r)));
            }
            if let Some(w) = m.data_width_bits {
                props.insert("Data Width (bits)".into(), PropertyValue::Integer(i64::from(w)));
            }
            if let Some(w) = m.total_width_bits {
                props.insert(
                    "Total Width (bits)".into(),
                    PropertyValue::Integer(i64::from(w)),
                );
            }

            out.inventory.push(InventoryNode {
                id: format!("memory_dev_{idx}"),
                parent_id: Some("memory_array".into()),
                kind: InventoryKind::MemoryDevice,
                label: format!(
                    "{}: {:.0} GB {} {}",
                    m.locator,
                    (m.size_bytes as f64) / (1024.0 * 1024.0 * 1024.0),
                    m.mem_type,
                    m.manufacturer
                ),
                properties: props,
                source: self.id().into(),
            });
        }

        out.summary_patch = Some(SummaryPatch::MemoryDmi {
            total_physical_bytes: mem.total_physical_bytes,
            max_capacity_bytes: mem.max_capacity_bytes,
            mem_type: mem.mem_type,
            configured_speed_mts: mem.configured_speed_mts,
            rated_speed_mts: mem.rated_speed_mts,
            channels_populated: mem.channels_populated,
            modules: mem.modules,
        });

        Ok(out)
    }
}

struct ParsedDmiMemory {
    total_physical_bytes: u64,
    max_capacity_bytes: Option<u64>,
    mem_type: Option<String>,
    configured_speed_mts: Option<u32>,
    rated_speed_mts: Option<u32>,
    channels_populated: u32,
    modules: Vec<MemoryModuleSummary>,
}

fn parse_udev_dmi() -> Option<ParsedDmiMemory> {
    let out = Command::new("udevadm")
        .args(["info", "--query=property", "--path=/sys/class/dmi/id"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let num_devices: usize = kv.get("MEMORY_ARRAY_NUM_DEVICES")?.parse().ok()?;
    let max_capacity_bytes = kv
        .get("MEMORY_ARRAY_MAX_CAPACITY")
        .and_then(|s| s.parse::<u64>().ok());

    let mut modules = Vec::new();
    let mut total_physical_bytes = 0u64;

    for i in 0..num_devices {
        let p = format!("MEMORY_DEVICE_{i}_");
        let size_bytes = kv
            .get(&format!("{p}SIZE"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if size_bytes == 0 {
            continue;
        }
        total_physical_bytes = total_physical_bytes.saturating_add(size_bytes);

        let voltage_mv = kv
            .get(&format!("{p}CONFIGURED_VOLTAGE"))
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| {
                if v < 20.0 {
                    (v * 1000.0) as u32
                } else {
                    v as u32
                }
            });

        modules.push(MemoryModuleSummary {
            locator: kv
                .get(&format!("{p}LOCATOR"))
                .cloned()
                .unwrap_or_else(|| format!("Slot {i}")),
            bank_locator: kv.get(&format!("{p}BANK_LOCATOR")).cloned().unwrap_or_default(),
            size_bytes,
            mem_type: kv
                .get(&format!("{p}TYPE"))
                .cloned()
                .unwrap_or_else(|| "DRAM".into()),
            form_factor: kv
                .get(&format!("{p}FORM_FACTOR"))
                .cloned()
                .unwrap_or_default(),
            speed_mts: kv
                .get(&format!("{p}SPEED_MTS"))
                .and_then(|s| s.parse::<u32>().ok()),
            configured_speed_mts: kv
                .get(&format!("{p}CONFIGURED_SPEED_MTS"))
                .and_then(|s| s.parse::<u32>().ok()),
            manufacturer: kv.get(&format!("{p}MANUFACTURER")).cloned().unwrap_or_default(),
            part_number: kv
                .get(&format!("{p}PART_NUMBER"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            voltage_mv,
            rank: kv.get(&format!("{p}RANK")).and_then(|s| s.parse::<u8>().ok()),
            data_width_bits: kv
                .get(&format!("{p}DATA_WIDTH"))
                .and_then(|s| s.parse::<u16>().ok()),
            total_width_bits: kv
                .get(&format!("{p}TOTAL_WIDTH"))
                .and_then(|s| s.parse::<u16>().ok()),
        });
    }

    if modules.is_empty() {
        return None;
    }

    let first = modules.first();
    Some(ParsedDmiMemory {
        total_physical_bytes,
        max_capacity_bytes,
        mem_type: first.map(|m| m.mem_type.clone()),
        configured_speed_mts: first.and_then(|m| m.configured_speed_mts),
        rated_speed_mts: first.and_then(|m| m.speed_mts),
        channels_populated: u32::try_from(modules.len()).unwrap_or(0),
        modules,
    })
}

fn parse_dmidecode() -> Option<ParsedDmiMemory> {
    let bin = if std::path::Path::new("/usr/sbin/dmidecode").exists() {
        "/usr/sbin/dmidecode"
    } else {
        "dmidecode"
    };
    let out = Command::new(bin).args(["-t", "memory"]).output().ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut max_capacity_bytes = None;
    let mut modules = Vec::new();
    let mut total_physical_bytes = 0u64;

    for block in text.split("\n\n") {
        if block.contains("Physical Memory Array") {
            for line in block.lines() {
                if let Some(val) = line.trim().strip_prefix("Maximum Capacity:") {
                    max_capacity_bytes = parse_size_bytes(val.trim());
                }
            }
        } else if block.contains("Memory Device") && !block.contains("Memory Device Mapped") {
            let mut m = MemoryModuleSummary::default();
            for line in block.lines() {
                let Some((k, v)) = line.trim().split_once(':') else {
                    continue;
                };
                let val = v.trim();
                match k.trim() {
                    "Size" => m.size_bytes = parse_size_bytes(val).unwrap_or(0),
                    "Locator" => m.locator = val.to_string(),
                    "Bank Locator" => m.bank_locator = val.to_string(),
                    "Type" => m.mem_type = val.to_string(),
                    "Form Factor" => m.form_factor = val.to_string(),
                    "Speed" => m.speed_mts = parse_leading_u32(val),
                    "Configured Memory Speed" => m.configured_speed_mts = parse_leading_u32(val),
                    "Manufacturer" => m.manufacturer = val.to_string(),
                    "Part Number" => m.part_number = val.to_string(),
                    "Rank" => m.rank = val.parse::<u8>().ok(),
                    "Data Width" => m.data_width_bits = parse_leading_u32(val).and_then(|n| u16::try_from(n).ok()),
                    "Total Width" => m.total_width_bits = parse_leading_u32(val).and_then(|n| u16::try_from(n).ok()),
                    "Configured Voltage" => {
                        if let Some(v_str) = val.split_whitespace().next() {
                            if let Ok(volts) = v_str.parse::<f64>() {
                                m.voltage_mv = Some((volts * 1000.0).round() as u32);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if m.size_bytes > 0 {
                total_physical_bytes = total_physical_bytes.saturating_add(m.size_bytes);
                modules.push(m);
            }
        }
    }

    if modules.is_empty() {
        return None;
    }

    let first = modules.first();
    Some(ParsedDmiMemory {
        total_physical_bytes,
        max_capacity_bytes,
        mem_type: first.map(|m| m.mem_type.clone()),
        configured_speed_mts: first.and_then(|m| m.configured_speed_mts),
        rated_speed_mts: first.and_then(|m| m.speed_mts),
        channels_populated: u32::try_from(modules.len()).unwrap_or(0),
        modules,
    })
}

fn parse_size_bytes(s: &str) -> Option<u64> {
    let mut parts = s.split_whitespace();
    let num: u64 = parts.next()?.parse().ok()?;
    let unit = parts.next().unwrap_or("");
    let mult = match unit {
        "KB" | "KiB" => 1024u64,
        "MB" | "MiB" => 1024 * 1024,
        "GB" | "GiB" => 1024 * 1024 * 1024,
        "TB" | "TiB" => 1024 * 1024 * 1024 * 1024,
        _ => return None,
    };
    Some(num.saturating_mul(mult))
}

fn parse_leading_u32(s: &str) -> Option<u32> {
    s.split_whitespace().next()?.parse::<u32>().ok()
}
