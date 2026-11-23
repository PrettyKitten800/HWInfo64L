//! Versioned v1 data schema, JSON-RPC 2.0 wire types, and NDJSON plugin
//! protocol types for HWInfo64L.
//!
//! All types in this crate are strictly additive within protocol version 1.

pub mod inventory;
pub mod plugin;
pub mod rpc;
pub mod sensor;

pub use inventory::{
    CpuSummary, DriveSummary, GpuSummary, InventoryKind, InventoryNode, MemoryModuleSummary,
    MemorySummary, MotherboardSummary, OsSummary, PropertyValue, SystemSummary,
};
pub use plugin::{PluginFromHost, PluginManifest, PluginToHost};
pub use rpc::{
    HelloResponse, HistoryResponse, JsonRpcError, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, ListSensorsResponse, ResetStatsResponse, SensorBatchParams,
    DEFAULT_SOCKET_PATH, PROTOCOL_VERSION,
};
pub use sensor::{
    HistorySample, PollTier, ProbeHealth, ProbeState, SensorDescriptor, SensorKind, SensorReading,
    SensorStats, SensorStatus,
};
