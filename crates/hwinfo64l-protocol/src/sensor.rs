use serde::{Deserialize, Serialize};

/// Category of a live sensor metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorKind {
    Temperature,
    Clock,
    Voltage,
    Power,
    Current,
    FanRpm,
    Utilization,
    Memory,
    Throughput,
    Ratio,
    BooleanFlag,
    Counter,
    Custom,
}

/// Polling cadence class for a probe or sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollTier {
    Static,
    Fast,
    Slow,
}

/// Health/freshness state of an individual reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorStatus {
    Ok,
    Stale,
    Unavailable,
    Warning,
    Critical,
}

/// Static metadata describing a sensor. Emitted on discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorDescriptor {
    /// Stable hierarchical ID: `<provider>.<device_key>.<category>.<metric>`.
    pub id: String,
    /// Display group in UI (e.g., `"CPU [#0]: Intel Celeron N5105"`).
    pub group: String,
    /// Human-readable sensor label (e.g., `"Package"`).
    pub name: String,
    /// Metric kind.
    pub kind: SensorKind,
    /// Display unit (e.g., `"°C"`, `"MHz"`, `"W"`, `"%"`, `"MiB"`).
    pub unit: String,
    /// Optional expected lower bound (useful for graph scaling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_expected: Option<f64>,
    /// Optional expected upper bound (useful for graph scaling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_expected: Option<f64>,
    /// Optional warning threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_threshold: Option<f64>,
    /// Optional critical threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crit_threshold: Option<f64>,
    /// Polling cadence tier.
    pub poll_tier: PollTier,
    /// Name of the built-in probe or external plugin supplying this sensor.
    pub source: String,
}

/// Running statistics maintained in O(1) by the core engine.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SensorStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub samples: u64,
}

/// Live value + statistics snapshot for a single sensor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorReading {
    pub id: String,
    /// Unix epoch milliseconds when the sample was captured.
    pub timestamp_ms: u64,
    pub value: f64,
    pub status: SensorStatus,
    pub stats: SensorStats,
}

/// Compact historical point stored in the per-sensor ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistorySample {
    pub timestamp_ms: u64,
    pub value: f64,
}

/// Operational health state of a supervised probe or plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeState {
    Healthy,
    Degraded,
    CircuitOpen,
    RequiresPrivilege,
    Unsupported,
}

/// Diagnostic telemetry for a single probe or external plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeHealth {
    pub probe_id: String,
    pub display_name: String,
    pub state: ProbeState,
    pub poll_tier: PollTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_ms: Option<u64>,
    pub last_poll_duration_us: u64,
    pub consecutive_errors: u32,
    pub total_errors: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}
