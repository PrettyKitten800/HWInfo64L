//! Fault-isolated core engine for HWInfo64L.
//!
//! Owns probe supervision, circuit breaking, bounded ring-buffer storage,
//! running statistics (min/max/avg), NDJSON external plugin hosting, and
//! HWiNFO64-compatible CSV logging.

pub mod csv_logger;
pub mod plugin;
pub mod probe;
pub mod store;
pub mod supervisor;

pub use csv_logger::CsvLogger;
pub use plugin::{discover_plugin_executables, NdjsonPluginProbe};
pub use probe::{now_epoch_ms, Probe, ProbeError, ProbeOutput, RawSample};
pub use store::{SensorStore, StoreConfig};
pub use supervisor::{CoreEngine, SupervisorConfig};
