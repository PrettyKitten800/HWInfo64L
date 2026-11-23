use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hwinfo64l_protocol::{InventoryNode, PollTier, SensorDescriptor, SensorStatus, SystemSummary};
use thiserror::Error;

/// Current Unix timestamp in milliseconds. Never panics even if system clock is pre-epoch.
pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Typed probe failure categories used by the supervisor's circuit breaker.
#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("requires elevated privileges: {0}")]
    RequiresPrivilege(String),

    #[error("unsupported on this system: {0}")]
    Unsupported(String),

    #[error("probe timed out after {0:?}")]
    Timeout(Duration),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("probe failed: {0}")]
    Transient(String),
}

/// Raw sample emitted by a probe before statistics aggregation.
#[derive(Debug, Clone)]
pub struct RawSample {
    pub id: String,
    pub value: f64,
    pub timestamp_ms: u64,
    pub status_override: Option<SensorStatus>,
}

impl RawSample {
    pub fn new(id: impl Into<String>, value: f64, timestamp_ms: u64) -> Self {
        Self {
            id: id.into(),
            value,
            timestamp_ms,
            status_override: None,
        }
    }
}

/// Output returned by a single probe poll cycle.
#[derive(Debug, Default, Clone)]
pub struct ProbeOutput {
    pub descriptors: Vec<SensorDescriptor>,
    pub samples: Vec<RawSample>,
    pub inventory: Vec<InventoryNode>,
    /// Optional partial summary patch applied by probes that know system summary fields.
    pub summary_patch: Option<SummaryPatch>,
}

/// Strongly typed summary update emitted by built-in inventory probes.
#[derive(Debug, Clone)]
pub enum SummaryPatch {
    Full(Box<SystemSummary>),
    Hostname(String),
    Os(hwinfo64l_protocol::OsSummary),
    Cpu(Box<hwinfo64l_protocol::CpuSummary>),
    CpuPowerLimits {
        pl1_watts: Option<f64>,
        pl2_watts: Option<f64>,
    },
    Motherboard(Box<hwinfo64l_protocol::MotherboardSummary>),
    MemoryRuntime {
        usable_bytes: u64,
        swap_total_bytes: u64,
        zswap_enabled: bool,
        zswap_compressor: Option<String>,
    },
    MemoryDmi {
        total_physical_bytes: u64,
        max_capacity_bytes: Option<u64>,
        mem_type: Option<String>,
        configured_speed_mts: Option<u32>,
        rated_speed_mts: Option<u32>,
        channels_populated: u32,
        modules: Vec<hwinfo64l_protocol::MemoryModuleSummary>,
    },
    Gpus(Vec<hwinfo64l_protocol::GpuSummary>),
    Drives(Vec<hwinfo64l_protocol::DriveSummary>),
    DriveSmartHealth {
        device: String,
        health: String,
    },
}

/// Core hardware collector contract.
///
/// Every built-in probe and external plugin implements this trait. The
/// supervisor invokes `poll` inside an isolated blocking worker with a hard
/// timeout and panic boundary, so implementations stay simple and synchronous
/// while remaining completely fault-isolated.
pub trait Probe: Send + 'static {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn poll_tier(&self) -> PollTier;

    /// Desired polling interval. `PollTier::Static` probes are polled once at startup
    /// (and optionally refreshed slowly if non-zero).
    fn poll_interval(&self) -> Duration {
        match self.poll_tier() {
            PollTier::Static => Duration::from_secs(0),
            PollTier::Fast => Duration::from_millis(1000),
            PollTier::Slow => Duration::from_secs(10),
        }
    }

    /// Maximum wall-clock duration allowed for a single `poll()` invocation.
    fn timeout(&self) -> Duration {
        match self.poll_tier() {
            PollTier::Static => Duration::from_secs(5),
            PollTier::Fast => Duration::from_millis(1500),
            PollTier::Slow => Duration::from_secs(4),
        }
    }

    fn poll(&mut self) -> Result<ProbeOutput, ProbeError>;
}
