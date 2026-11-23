use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Category of a hardware inventory tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryKind {
    System,
    Cpu,
    Motherboard,
    Bios,
    MemoryArray,
    MemoryDevice,
    Gpu,
    Storage,
    Network,
    PciDevice,
    UsbDevice,
    OperatingSystem,
    Plugin,
}

/// Typed property value inside an `InventoryNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Text(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    List(Vec<String>),
}

/// Node in the HWiNFO64-style hardware tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub kind: InventoryKind,
    pub label: String,
    #[serde(default)]
    pub properties: BTreeMap<String, PropertyValue>,
    pub source: String,
}

/// High-level System Summary matching HWiNFO64's Summary Panel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemSummary {
    pub hostname: String,
    pub os: OsSummary,
    pub cpu: CpuSummary,
    pub motherboard: MotherboardSummary,
    pub memory: MemorySummary,
    pub gpus: Vec<GpuSummary>,
    pub drives: Vec<DriveSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OsSummary {
    pub pretty_name: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuSummary {
    pub model_name: String,
    pub vendor: String,
    pub physical_cores: u32,
    pub logical_cpus: u32,
    pub min_mhz: Option<f64>,
    pub max_mhz: Option<f64>,
    pub pl1_watts: Option<f64>,
    pub pl2_watts: Option<f64>,
    pub l1_cache: Option<String>,
    pub l2_cache: Option<String>,
    pub l3_cache: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MotherboardSummary {
    pub sys_vendor: String,
    pub product_name: String,
    pub board_vendor: String,
    pub board_name: String,
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemorySummary {
    pub total_physical_bytes: u64,
    pub usable_bytes: u64,
    pub swap_total_bytes: u64,
    pub zswap_enabled: bool,
    pub zswap_compressor: Option<String>,
    pub mem_type: Option<String>,
    pub configured_speed_mts: Option<u32>,
    pub rated_speed_mts: Option<u32>,
    pub channels_populated: u32,
    pub max_capacity_bytes: Option<u64>,
    pub modules: Vec<MemoryModuleSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryModuleSummary {
    pub locator: String,
    pub bank_locator: String,
    pub size_bytes: u64,
    pub mem_type: String,
    pub form_factor: String,
    pub speed_mts: Option<u32>,
    pub configured_speed_mts: Option<u32>,
    pub manufacturer: String,
    pub part_number: String,
    pub voltage_mv: Option<u32>,
    pub rank: Option<u8>,
    pub data_width_bits: Option<u16>,
    pub total_width_bits: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuSummary {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub driver: String,
    pub pci_slot: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DriveSummary {
    pub device: String,
    pub model: String,
    pub transport: String,
    pub size_bytes: u64,
    pub rotational: bool,
    pub smart_health: Option<String>,
}
