//! Modular built-in Linux hardware & sensor probes for HWInfo64L.
//!
//! Each probe implements `hwinfo64l_core::Probe` and runs isolated under the
//! supervisor. Direct kernel interfaces (`sysfs`, `procfs`) are preferred for
//! microsecond latency and zero subprocess overhead.

pub mod cpu;
pub mod dmi_memory;
pub mod gpu;
pub mod hwmon;
pub mod memory;
pub mod network;
pub mod rapl;
pub mod smart;
pub mod system;
pub(crate) mod util;

use hwinfo64l_core::Probe;

/// Return the standard suite of Linux probes.
pub fn default_probes() -> Vec<Box<dyn Probe>> {
    vec![
        Box::new(system::SystemInventoryProbe::new()),
        Box::new(dmi_memory::DmiMemoryProbe::new()),
        Box::new(hwmon::HwmonProbe::new()),
        Box::new(cpu::CpuTelemetryProbe::new()),
        Box::new(rapl::RaplProbe::new()),
        Box::new(memory::MemoryProbe::new()),
        Box::new(gpu::GpuProbe::new()),
        Box::new(network::NetworkProbe::new()),
        Box::new(smart::SmartProbe::new()),
    ]
}
