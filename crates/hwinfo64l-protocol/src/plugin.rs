use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::inventory::InventoryNode;
use crate::sensor::SensorDescriptor;

/// Messages sent from the HWInfo64L host to an external NDJSON plugin over `stdin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginFromHost {
    Hello { protocol_version: u32 },
    Poll,
    Shutdown,
}

/// Plugin capability & metadata declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Suggested polling interval in milliseconds (clamped by host).
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
    #[serde(default)]
    pub sensors: Vec<SensorDescriptor>,
    #[serde(default)]
    pub inventory: Vec<InventoryNode>,
}

fn default_interval_ms() -> u64 {
    1000
}

/// Messages sent from an external NDJSON plugin to the HWInfo64L host over `stdout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginToHost {
    Manifest(PluginManifest),
    Readings {
        timestamp_ms: u64,
        values: BTreeMap<String, f64>,
    },
    Inventory {
        nodes: Vec<InventoryNode>,
    },
    Error {
        message: String,
    },
}
