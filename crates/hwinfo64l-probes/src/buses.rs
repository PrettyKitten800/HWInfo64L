use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use hwinfo64l_core::{Probe, ProbeError, ProbeOutput};
use hwinfo64l_protocol::{InventoryKind, InventoryNode, PollTier, PropertyValue};

use crate::util::{read_trimmed, resolve_trusted_binary, sanitize_key};

/// Static probe enumerating PCIe devices (with link speed & lane width) and
/// USB devices into the Hardware Tree.
pub struct BusTopologyProbe;

impl BusTopologyProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BusTopologyProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for BusTopologyProbe {
    fn id(&self) -> &str {
        "bus_topology"
    }

    fn display_name(&self) -> &str {
        "PCIe & USB Bus Topology"
    }

    fn poll_tier(&self) -> PollTier {
        PollTier::Static
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError> {
        let mut out = ProbeOutput::default();
        collect_pci(&mut out, self.id());
        collect_usb(&mut out, self.id());
        if out.inventory.is_empty() {
            return Err(ProbeError::Unsupported("no PCI or USB devices found".into()));
        }
        Ok(out)
    }
}

fn collect_pci(out: &mut ProbeOutput, source: &str) {
    let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") else {
        return;
    };

    let lspci_names = load_lspci_names();
    let mut slots: Vec<_> = entries.flatten().collect();
    slots.sort_by_key(|e| e.file_name());

    if slots.is_empty() {
        return;
    }

    let mut bus_props = BTreeMap::new();
    bus_props.insert(
        "Device Count".into(),
        PropertyValue::Integer(i64::try_from(slots.len()).unwrap_or(0)),
    );
    out.inventory.push(InventoryNode {
        id: "pci_bus".into(),
        parent_id: Some("system".into()),
        kind: InventoryKind::PciDevice,
        label: format!("PCI / PCIe Bus ({} devices)", slots.len()),
        properties: bus_props,
        source: source.into(),
    });

    for entry in slots {
        let slot = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let short_slot = slot.strip_prefix("0000:").unwrap_or(&slot);

        let vendor = read_trimmed(path.join("vendor")).unwrap_or_default();
        let device = read_trimmed(path.join("device")).unwrap_or_default();
        let class = read_trimmed(path.join("class")).unwrap_or_default();
        let driver = fs::read_link(path.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()));
        let cur_speed = read_trimmed(path.join("current_link_speed"));
        let cur_width = read_trimmed(path.join("current_link_width"));
        let max_speed = read_trimmed(path.join("max_link_speed"));
        let max_width = read_trimmed(path.join("max_link_width"));

        let friendly = lspci_names
            .get(short_slot)
            .or_else(|| lspci_names.get(&slot))
            .cloned()
            .unwrap_or_else(|| format!("PCI {vendor}:{device}"));

        let mut props = BTreeMap::new();
        props.insert("PCI Address".into(), PropertyValue::Text(slot.clone()));
        props.insert("Vendor:Device".into(), PropertyValue::Text(format!("{vendor}:{device}")));
        props.insert("Class".into(), PropertyValue::Text(class));
        if let Some(drv) = driver {
            props.insert("Kernel Driver".into(), PropertyValue::Text(drv));
        }
        if let (Some(spd), Some(wid)) = (&cur_speed, &cur_width) {
            if spd != "Unknown" && wid != "0" {
                props.insert(
                    "Current PCIe Link".into(),
                    PropertyValue::Text(format!("{spd} x{wid}")),
                );
            }
        }
        if let (Some(spd), Some(wid)) = (&max_speed, &max_width) {
            if spd != "Unknown" && wid != "0" {
                props.insert(
                    "Max PCIe Link".into(),
                    PropertyValue::Text(format!("{spd} x{wid}")),
                );
            }
        }

        let link_suffix = match (&cur_speed, &cur_width) {
            (Some(s), Some(w)) if s != "Unknown" && w != "0" => format!(" [{s} x{w}]"),
            _ => String::new(),
        };

        out.inventory.push(InventoryNode {
            id: format!("pci_{}", sanitize_key(&slot)),
            parent_id: Some("pci_bus".into()),
            kind: InventoryKind::PciDevice,
            label: format!("{short_slot} — {friendly}{link_suffix}"),
            properties: props,
            source: source.into(),
        });
    }
}

fn load_lspci_names() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(bin) = resolve_trusted_binary("lspci") else {
        return map;
    };
    let Ok(out) = Command::new(bin).output() else {
        return map;
    };
    if !out.status.success() {
        return map;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((slot, rest)) = line.split_once(' ') {
            map.insert(slot.trim().to_string(), rest.trim().to_string());
        }
    }
    map
}

fn collect_usb(out: &mut ProbeOutput, source: &str) {
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return;
    };

    let mut devices = Vec::new();
    let mut dirs: Vec<_> = entries.flatten().collect();
    dirs.sort_by_key(|e| e.file_name());

    for entry in dirs {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip USB interface entries like "1-2:1.0"
        if name.contains(':') {
            continue;
        }
        let path = entry.path();
        let Some(vid) = read_trimmed(path.join("idVendor")) else {
            continue;
        };
        let Some(pid) = read_trimmed(path.join("idProduct")) else {
            continue;
        };
        let product = read_trimmed(path.join("product")).unwrap_or_else(|| format!("USB {vid}:{pid}"));
        let manufacturer = read_trimmed(path.join("manufacturer")).unwrap_or_default();
        let speed = read_trimmed(path.join("speed"));
        let version = read_trimmed(path.join("version"));

        let mut props = BTreeMap::new();
        props.insert("Bus Path".into(), PropertyValue::Text(name.clone()));
        props.insert("VID:PID".into(), PropertyValue::Text(format!("{vid}:{pid}")));
        if !manufacturer.is_empty() {
            props.insert("Manufacturer".into(), PropertyValue::Text(manufacturer.clone()));
        }
        props.insert("Product".into(), PropertyValue::Text(product.clone()));
        if let Some(ref s) = speed {
            props.insert("Link Speed".into(), PropertyValue::Text(format!("{s} Mbps")));
        }
        if let Some(ref v) = version {
            props.insert("USB Spec".into(), PropertyValue::Text(format!("USB {v}")));
        }

        let label = if manufacturer.is_empty() {
            format!("{product} ({vid}:{pid})")
        } else {
            format!("{manufacturer} {product} ({vid}:{pid})")
        };

        devices.push(InventoryNode {
            id: format!("usb_{}", sanitize_key(&name)),
            parent_id: Some("usb_bus".into()),
            kind: InventoryKind::UsbDevice,
            label,
            properties: props,
            source: source.into(),
        });
    }

    if devices.is_empty() {
        return;
    }

    let mut bus_props = BTreeMap::new();
    bus_props.insert(
        "Device Count".into(),
        PropertyValue::Integer(i64::try_from(devices.len()).unwrap_or(0)),
    );
    out.inventory.push(InventoryNode {
        id: "usb_bus".into(),
        parent_id: Some("system".into()),
        kind: InventoryKind::UsbDevice,
        label: format!("USB Bus ({} devices)", devices.len()),
        properties: bus_props,
        source: source.into(),
    });
    out.inventory.extend(devices);
}
